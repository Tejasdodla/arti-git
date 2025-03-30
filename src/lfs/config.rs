/// LFS configuration settings
use std::path::{Path, PathBuf};
use serde::{Serialize, Deserialize};

/// Configuration for Git LFS functionality
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LfsConfig {
    /// Whether LFS is enabled
    #[serde(default = "default_lfs_enabled")]
    pub enabled: bool,
    
    /// Whether to use IPFS for storage
    #[serde(default = "default_use_ipfs")]
    pub use_ipfs: bool,
    
    /// LFS server URL
    #[serde(default)]
    pub url: Option<String>,
    
    /// Base directory for local LFS objects
    #[serde(default = "default_lfs_dir")]
    pub objects_dir: PathBuf,
    
    /// Size threshold for automatic LFS tracking (in bytes)
    #[serde(default = "default_size_threshold")]
    pub size_threshold: u64,
    
    /// File patterns to track with LFS
    #[serde(default)]
    pub track_patterns: Vec<String>,
    
    /// Whether to pin LFS objects in IPFS
    #[serde(default = "default_pin_objects")]
    pub pin_objects: bool,
    
    /// Whether IPFS is the primary storage (if false, local storage is primary)
    #[serde(default)]
    pub ipfs_primary: bool,
    
    /// Whether to automatically upload objects to IPFS when downloaded from LFS server
    #[serde(default = "default_auto_upload")]
    pub auto_upload_to_ipfs: bool,
}

fn default_lfs_enabled() -> bool {
    true
}

fn default_use_ipfs() -> bool {
    false
}

fn default_lfs_dir() -> PathBuf {
    Path::new(".git").join("lfs").join("objects")
}

fn default_size_threshold() -> u64 {
    // Default to 10MB
    10 * 1024 * 1024
}

fn default_pin_objects() -> bool {
    true
}

fn default_auto_upload() -> bool {
    true
}

impl Default for LfsConfig {
    fn default() -> Self {
        Self {
            enabled: default_lfs_enabled(),
            use_ipfs: default_use_ipfs(),
            url: None,
            objects_dir: default_lfs_dir(),
            size_threshold: default_size_threshold(),
            track_patterns: Vec::new(),
            pin_objects: default_pin_objects(),
            ipfs_primary: false,
            auto_upload_to_ipfs: default_auto_upload(),
        }
    }
}

impl LfsConfig {
    /// Create a new LFS configuration with default settings
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Enable or disable LFS
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    
    /// Enable or disable IPFS integration
    pub fn with_ipfs(mut self, use_ipfs: bool) -> Self {
        self.use_ipfs = use_ipfs;
        self
    }
    
    /// Set the LFS server URL
    pub fn with_url(mut self, url: Option<String>) -> Self {
        self.url = url;
        self
    }
    
    /// Set the path to the LFS objects directory
    pub fn with_objects_dir(mut self, objects_dir: PathBuf) -> Self {
        self.objects_dir = objects_dir;
        self
    }
    
    /// Set the size threshold for automatic LFS tracking
    pub fn with_size_threshold(mut self, size_threshold: u64) -> Self {
        self.size_threshold = size_threshold;
        self
    }
    
    /// Add a pattern to track with LFS
    pub fn add_track_pattern(mut self, pattern: String) -> Self {
        self.track_patterns.push(pattern);
        self
    }
    
    /// Set whether to pin objects in IPFS
    pub fn with_pin_objects(mut self, pin_objects: bool) -> Self {
        self.pin_objects = pin_objects;
        self
    }
    
    /// Set whether IPFS is the primary storage
    pub fn with_ipfs_primary(mut self, ipfs_primary: bool) -> Self {
        self.ipfs_primary = ipfs_primary;
        self
    }
    
    /// Set whether to automatically upload objects to IPFS when downloaded from LFS server
    pub fn with_auto_upload_to_ipfs(mut self, auto_upload_to_ipfs: bool) -> Self {
        self.auto_upload_to_ipfs = auto_upload_to_ipfs;
        self
    }
    
    /// Get the absolute path to the LFS objects directory
    pub fn get_absolute_objects_dir(&self, repo_path: impl AsRef<Path>) -> PathBuf {
        let repo_path = repo_path.as_ref();
        
        if self.objects_dir.is_absolute() {
            self.objects_dir.clone()
        } else {
            repo_path.join(&self.objects_dir)
        }
    }
    
    /// Check if a file should be tracked based on its path
    pub fn should_track(&self, path: impl AsRef<Path>) -> bool {
        let path = path.as_ref();
        let path_str = path.to_string_lossy();
        
        for pattern in &self.track_patterns {
            if let Ok(glob) = glob::Pattern::new(pattern) {
                if glob.matches(&path_str) {
                    return true;
                }
            }
        }
        
        false
    }
}