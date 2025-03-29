use std::path::PathBuf;
use serde::{Serialize, Deserialize};

/// Configuration for Git LFS with IPFS integration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LfsConfig {
    /// Whether LFS is enabled
    pub enabled: bool,
    
    /// The threshold file size in bytes to automatically track with LFS
    pub size_threshold: u64,
    
    /// File patterns to track with LFS
    pub track_patterns: Vec<String>,
    
    /// The directory where LFS objects are stored
    pub objects_dir: PathBuf,
    
    /// Whether to use IPFS for object storage
    pub use_ipfs: bool,
    
    /// The IPFS gateway URL
    pub ipfs_gateway: Option<String>,
    
    /// Whether to pin objects on IPFS
    pub ipfs_pin: bool,
}

impl Default for LfsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            size_threshold: 5 * 1024 * 1024, // 5MB default
            track_patterns: Vec::new(),
            objects_dir: PathBuf::from(".git/lfs/objects"),
            use_ipfs: false,
            ipfs_gateway: None,
            ipfs_pin: true,
        }
    }
}

impl LfsConfig {
    /// Create a new default LFS configuration
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Enable LFS
    pub fn enable(&mut self) {
        self.enabled = true;
    }
    
    /// Disable LFS
    pub fn disable(&mut self) {
        self.enabled = false;
    }
    
    /// Set the size threshold for automatic tracking
    pub fn set_size_threshold(&mut self, threshold: u64) {
        self.size_threshold = threshold;
    }
    
    /// Add a pattern to track with LFS
    pub fn add_track_pattern(&mut self, pattern: &str) {
        self.track_patterns.push(pattern.to_string());
    }
    
    /// Remove a pattern from LFS tracking
    pub fn remove_track_pattern(&mut self, pattern: &str) {
        self.track_patterns.retain(|p| p != pattern);
    }
    
    /// Set the directory for LFS objects
    pub fn set_objects_dir(&mut self, dir: PathBuf) {
        self.objects_dir = dir;
    }
    
    /// Enable IPFS integration
    pub fn enable_ipfs(&mut self, gateway_url: &str) {
        self.use_ipfs = true;
        self.ipfs_gateway = Some(gateway_url.to_string());
    }
    
    /// Disable IPFS integration
    pub fn disable_ipfs(&mut self) {
        self.use_ipfs = false;
    }
    
    /// Set whether to pin objects on IPFS
    pub fn set_ipfs_pin(&mut self, pin: bool) {
        self.ipfs_pin = pin;
    }
    
    /// Load configuration from a git config file
    pub fn from_git_config(config: &gix_config::File) -> Self {
        let mut lfs_config = Self::default();
        
        // Check if LFS is enabled
        if let Ok(enabled) = config.boolean("lfs.enabled") {
            lfs_config.enabled = enabled;
        }
        
        // Get the size threshold
        if let Ok(threshold) = config.integer("lfs.sizethreshold") {
            lfs_config.size_threshold = threshold as u64;
        }
        
        // Get tracked patterns
        if let Ok(patterns) = config.string("lfs.trackpatterns") {
            lfs_config.track_patterns = patterns
                .split(',')
                .map(|s| s.trim().to_string())
                .collect();
        }
        
        // Get objects directory
        if let Ok(dir) = config.string("lfs.objectsdir") {
            lfs_config.objects_dir = PathBuf::from(dir);
        }
        
        // Check if IPFS is enabled
        if let Ok(use_ipfs) = config.boolean("lfs.ipfs.enabled") {
            lfs_config.use_ipfs = use_ipfs;
        }
        
        // Get IPFS gateway
        if let Ok(gateway) = config.string("lfs.ipfs.gateway") {
            lfs_config.ipfs_gateway = Some(gateway);
        }
        
        // Check if IPFS pinning is enabled
        if let Ok(pin) = config.boolean("lfs.ipfs.pin") {
            lfs_config.ipfs_pin = pin;
        }
        
        lfs_config
    }
    
    /// Save configuration to a git config file
    pub fn to_git_config(&self, config: &mut gix_config::File) -> std::io::Result<()> {
        // Set LFS enabled
        config.set_boolean("lfs.enabled", self.enabled)?;
        
        // Set size threshold
        config.set_integer("lfs.sizethreshold", self.size_threshold as i64)?;
        
        // Set tracked patterns
        if !self.track_patterns.is_empty() {
            let patterns = self.track_patterns.join(",");
            config.set_str("lfs.trackpatterns", &patterns)?;
        }
        
        // Set objects directory
        config.set_str(
            "lfs.objectsdir", 
            self.objects_dir.to_string_lossy().as_ref()
        )?;
        
        // Set IPFS enabled
        config.set_boolean("lfs.ipfs.enabled", self.use_ipfs)?;
        
        // Set IPFS gateway
        if let Some(gateway) = &self.ipfs_gateway {
            config.set_str("lfs.ipfs.gateway", gateway)?;
        }
        
        // Set IPFS pinning
        config.set_boolean("lfs.ipfs.pin", self.ipfs_pin)?;
        
        Ok(())
    }
}