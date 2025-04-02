/// LFS object storage implementation
use std::path::{Path, PathBuf};
use std::fs;
use std::io;
use std::sync::Arc;
use std::collections::HashMap;
use async_trait::async_trait;
use tokio::fs as tokio_fs;
use bytes::Bytes;
use tokio::sync::RwLock;
use serde::{Serialize, Deserialize};

use crate::core::{GitError, Result, io_err};
use crate::ipfs::IpfsClient;
use crate::lfs::pointer::LfsPointer;

/// An LFS object ID, which is a SHA-256 hash
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LfsObjectId {
    /// The raw ID string (e.g., "sha256:abcdef123456...")
    id: String,
}

impl LfsObjectId {
    /// Create a new LFS object ID
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
        }
    }
    
    /// Get the ID as a string
    pub fn as_str(&self) -> &str {
        &self.id
    }
    
    /// Get the hash portion of the ID (removing the "sha256:" prefix)
    pub fn hash(&self) -> &str {
        if let Some(hash) = self.id.strip_prefix("sha256:") {
            hash
        } else {
            &self.id
        }
    }
    
    /// From a pointer file
    pub fn from_pointer(pointer: &LfsPointer) -> Self {
        Self::new(&pointer.oid)
    }
}

impl std::fmt::Display for LfsObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.id)
    }
}

impl From<&str> for LfsObjectId {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

/// LFS storage statistics 
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub struct LfsStorageStats {
    /// Total number of objects
    pub object_count: usize,
    /// Total size of all objects in bytes
    pub total_size: u64,
    /// Number of objects stored in IPFS
    pub ipfs_object_count: usize,
    /// Number of objects with local copies
    pub local_object_count: usize,
    /// Number of IPFS-only objects (not stored locally)
    pub ipfs_only_count: usize,
    /// Number of cache hits
    pub cache_hits: usize,
    /// Number of cache misses
    pub cache_misses: usize,
}

/// Details about a stored LFS object
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LfsObjectInfo {
    /// Object ID
    pub id: String,
    /// Size in bytes
    pub size: u64,
    /// IPFS CID if stored in IPFS
    pub ipfs_cid: Option<String>,
    /// Whether the object is stored locally
    pub is_local: bool,
    /// Original filename if known
    pub filename: Option<String>,
    /// MIME type if known
    pub mimetype: Option<String>,
    /// When the object was added
    #[serde(with = "chrono::serde::ts_seconds")]
    pub added_at: chrono::DateTime<chrono::Utc>,
}

/// Trait for LFS object providers (storage backends)
#[async_trait]
pub trait LfsObjectProvider: Send + Sync {
    /// Check if an object exists
    async fn has_object(&self, id: &LfsObjectId) -> bool;
    
    /// Get an object's data
    async fn get_object_bytes(&self, id: &LfsObjectId) -> Result<Bytes>;
    
    /// Store an object
    async fn store_object(&self, id: &LfsObjectId, data: &[u8]) -> Result<()>;
    
    /// Delete an object
    async fn delete_object(&self, id: &LfsObjectId) -> Result<()>;
    
    /// Get information about an object
    async fn get_object_info(&self, id: &LfsObjectId) -> Result<LfsObjectInfo>;
    
    /// Get storage statistics
    async fn get_stats(&self) -> Result<LfsStorageStats>;
}

/// Error type for LFS storage operations
#[derive(Debug)]
pub enum LfsStorageError {
    /// I/O error
    Io(io::Error),
    
    /// Object not found
    NotFound(String),
    
    /// Invalid object ID
    InvalidId(String),
    
    /// IPFS error
    IpfsError(String),
    
    /// Serialization error
    SerializationError(String),
}

impl From<io::Error> for LfsStorageError {
    fn from(err: io::Error) -> Self {
        LfsStorageError::Io(err)
    }
}

impl From<LfsStorageError> for GitError {
    fn from(err: LfsStorageError) -> Self {
        match err {
            LfsStorageError::Io(e) => GitError::IO(e.to_string(), None),
            LfsStorageError::NotFound(msg) => GitError::LfsError(msg),
            LfsStorageError::InvalidId(msg) => GitError::LfsError(msg),
            LfsStorageError::IpfsError(msg) => GitError::IpfsError(msg),
            LfsStorageError::SerializationError(msg) => GitError::LfsError(msg),
        }
    }
}

/// Object metadata for storage persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredObjectMetadata {
    /// Object ID
    id: String,
    /// Size in bytes
    size: u64,
    /// IPFS CID if stored in IPFS
    ipfs_cid: Option<String>,
    /// Original filename if known
    filename: Option<String>,
    /// MIME type if known
    mimetype: Option<String>,
    /// When the object was added
    #[serde(with = "chrono::serde::ts_seconds")]
    added_at: chrono::DateTime<chrono::Utc>,
}

impl From<StoredObjectMetadata> for LfsObjectInfo {
    fn from(meta: StoredObjectMetadata) -> Self {
        Self {
            id: meta.id,
            size: meta.size,
            ipfs_cid: meta.ipfs_cid,
            is_local: true, // This will be updated by the caller
            filename: meta.filename,
            mimetype: meta.mimetype,
            added_at: meta.added_at,
        }
    }
}

/// Main LFS storage implementation that can use multiple backends
pub struct LfsStorage {
    /// The base directory for object storage
    base_dir: PathBuf,
    
    /// IPFS client for IPFS-based storage (if enabled)
    ipfs_client: Option<Arc<IpfsClient>>,
    
    /// Whether IPFS is the primary storage backend
    ipfs_primary: bool,
    
    /// Whether to automatically pin objects in IPFS
    ipfs_pin: bool,
    
    /// Cache of object metadata
    metadata_cache: RwLock<HashMap<String, StoredObjectMetadata>>,
    
    /// Storage statistics
    stats: RwLock<LfsStorageStats>,
    
    /// Bandwidth throttling settings (bytes/sec, 0 = unlimited)
    upload_throttle: RwLock<u64>,
    download_throttle: RwLock<u64>,
}

impl LfsStorage {
    /// Create a new LFS storage with the given base directory
    pub fn new(base_dir: impl AsRef<Path>) -> Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();
        
        // Ensure the directory exists
        fs::create_dir_all(&base_dir)
            .map_err(|e| io_err(format!("Failed to create LFS storage directory: {}", e), &base_dir))?;
        
        // Also create the metadata directory
        let metadata_dir = base_dir.join("metadata");
        fs::create_dir_all(&metadata_dir)
            .map_err(|e| io_err(format!("Failed to create metadata directory: {}", e), &metadata_dir))?;
        
        let storage = Self {
            base_dir,
            ipfs_client: None,
            ipfs_primary: false,
            ipfs_pin: true,
            metadata_cache: RwLock::new(HashMap::new()),
            stats: RwLock::new(LfsStorageStats::default()),
            upload_throttle: RwLock::new(0),
            download_throttle: RwLock::new(0),
        };
        
        // Load existing metadata
        tokio::task::spawn(async move {
            if let Err(e) = storage.load_metadata().await {
                log::error!("Failed to load LFS metadata: {}", e);
            }
        });
        
        Ok(storage)
    }
    
    /// Create a new LFS storage with IPFS integration
    pub fn with_ipfs(base_dir: impl AsRef<Path>, ipfs_client: Arc<IpfsClient>, ipfs_primary: bool) -> Result<Self> {
        let mut storage = Self::new(base_dir)?;
        storage.ipfs_client = Some(ipfs_client);
        storage.ipfs_primary = ipfs_primary;
        Ok(storage)
    }
    
    /// Set bandwidth throttling for uploads (bytes/sec, 0 = unlimited)
    pub async fn set_upload_throttle(&self, bytes_per_sec: u64) {
        let mut throttle = self.upload_throttle.write().await;
        *throttle = bytes_per_sec;
        log::info!("LFS upload throttle set to {} bytes/sec", bytes_per_sec);
    }
    
    /// Set bandwidth throttling for downloads (bytes/sec, 0 = unlimited)
    pub async fn set_download_throttle(&self, bytes_per_sec: u64) {
        let mut throttle = self.download_throttle.write().await;
        *throttle = bytes_per_sec;
        log::info!("LFS download throttle set to {} bytes/sec", bytes_per_sec);
    }
    
    /// Load metadata from disk
    async fn load_metadata(&self) -> Result<()> {
        log::debug!("Loading LFS metadata...");
        
        let metadata_dir = self.base_dir.join("metadata");
        let mut entries = tokio_fs::read_dir(&metadata_dir).await
            .map_err(|e| io_err(format!("Failed to read metadata directory: {}", e), &metadata_dir))?;
        
        let mut count = 0;
        
        while let Some(entry) = entries.next_entry().await
            .map_err(|e| io_err(format!("Failed to read directory entry: {}", e), &metadata_dir))? {
            
            let path = entry.path();
            if !path.extension().map_or(false, |ext| ext == "json") {
                continue;
            }
            
            match tokio_fs::read_to_string(&path).await {
                Ok(content) => {
                    match serde_json::from_str::<StoredObjectMetadata>(&content) {
                        Ok(metadata) => {
                            let id = metadata.id.clone();
                            
                            // Update stats
                            {
                                let mut stats = self.stats.write().await;
                                stats.object_count += 1;
                                stats.total_size += metadata.size;
                                
                                if metadata.ipfs_cid.is_some() {
                                    stats.ipfs_object_count += 1;
                                }
                                
                                // We'll update local_object_count after checking if files exist
                            }
                            
                            // Check if the file exists locally
                            let object_path = self.get_object_path(&LfsObjectId::new(&id));
                            if object_path.exists() {
                                // Update stats
                                let mut stats = self.stats.write().await;
                                stats.local_object_count += 1;
                            } else if metadata.ipfs_cid.is_some() {
                                // Object is in IPFS but not locally
                                let mut stats = self.stats.write().await;
                                stats.ipfs_only_count += 1;
                            }
                            
                            // Update the cache
                            self.metadata_cache.write().await.insert(id, metadata);
                            count += 1;
                        },
                        Err(e) => {
                            log::warn!("Failed to parse metadata file {}: {}", path.display(), e);
                        }
                    }
                },
                Err(e) => {
                    log::warn!("Failed to read metadata file {}: {}", path.display(), e);
                }
            }
        }
        
        log::info!("Loaded metadata for {} LFS objects", count);
        Ok(())
    }
    
    /// Save metadata for an object
    async fn save_metadata(&self, id: &LfsObjectId, size: u64, ipfs_cid: Option<String>, 
                          filename: Option<String>, mimetype: Option<String>) -> Result<()> {
                              
        let metadata = StoredObjectMetadata {
            id: id.as_str().to_string(),
            size,
            ipfs_cid,
            filename,
            mimetype,
            added_at: chrono::Utc::now(),
        };
        
        // Update the cache
        self.metadata_cache.write().await.insert(id.as_str().to_string(), metadata.clone());
        
        // Save to disk
        let metadata_path = self.get_metadata_path(id);
        
        // Ensure the parent directory exists
        if let Some(parent) = metadata_path.parent() {
            tokio_fs::create_dir_all(parent).await
                .map_err(|e| io_err(format!("Failed to create directory: {}", e), parent))?;
        }
        
        // Serialize to JSON
        let json = serde_json::to_string_pretty(&metadata)
            .map_err(|e| GitError::LfsError(format!("Failed to serialize metadata: {}", e)))?;
        
        // Write to disk
        tokio_fs::write(&metadata_path, json).await
            .map_err(|e| io_err(format!("Failed to write metadata file: {}", e), &metadata_path))?;
        
        Ok(())
    }
    
    /// Get the path for an object's metadata
    fn get_metadata_path(&self, id: &LfsObjectId) -> PathBuf {
        let hash = id.hash();
        self.base_dir.join("metadata").join(format!("{}.json", hash))
    }
    
    /// Get the path for an object
    fn get_object_path(&self, id: &LfsObjectId) -> PathBuf {
        let hash = id.hash();
        
        // Use the first 2 characters as a directory prefix for better file distribution
        let prefix = &hash[0..2];
        let rest = &hash[2..];
        
        self.base_dir.join("objects").join(prefix).join(rest)
    }
    
    /// Store an object in the local filesystem
    async fn store_local(&self, id: &LfsObjectId, data: &[u8]) -> Result<()> {
        let path = self.get_object_path(id);
        
        // Ensure the parent directory exists
        if let Some(parent) = path.parent() {
            tokio_fs::create_dir_all(parent).await
                .map_err(|e| io_err(format!("Failed to create directory: {}", e), parent))?;
        }
        
        // Apply upload throttling if configured
        let throttle = *self.upload_throttle.read().await;
        if throttle > 0 && data.len() > 0 {
            // Calculate how long this upload should take with throttling
            let expected_secs = (data.len() as f64 / throttle as f64).ceil() as u64;
            if expected_secs > 0 {
                // Split data into chunks for throttled upload
                let chunk_size = (throttle / 10).max(1024) as usize; // At least 1KB chunks, aiming for 10 chunks per second
                let mut remaining = data;
                let mut file = tokio::fs::File::create(&path).await
                    .map_err(|e| io_err(format!("Failed to create file: {}", e), &path))?;
                
                while !remaining.is_empty() {
                    let chunk_size = std::cmp::min(chunk_size, remaining.len());
                    let (chunk, rest) = remaining.split_at(chunk_size);
                    
                    // Write chunk
                    tokio::io::AsyncWriteExt::write_all(&mut file, chunk).await
                        .map_err(|e| io_err(format!("Failed to write to file: {}", e), &path))?;
                    
                    // Throttle between chunks
                    if !rest.is_empty() {
                        let delay_ms = (1000.0 / 10.0) as u64; // Aim for 10 chunks per second
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                    }
                    
                    remaining = rest;
                }
            } else {
                // For small files, just write directly
                tokio_fs::write(&path, data).await
                    .map_err(|e| io_err(format!("Failed to write object file: {}", e), &path))?;
            }
        } else {
            // No throttling, write directly
            tokio_fs::write(&path, data).await
                .map_err(|e| io_err(format!("Failed to write object file: {}", e), &path))?;
        }
        
        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.local_object_count += 1;
        }
        
        Ok(())
    }
    
    /// Store an object in IPFS
    async fn store_ipfs(&self, id: &LfsObjectId, data: &[u8]) -> Result<String> {
        let ipfs_client = self.ipfs_client.as_ref()
            .ok_or_else(|| GitError::LfsError("IPFS client not configured".to_string()))?;
        
        // Upload to IPFS
        log::debug!("Storing LFS object {} in IPFS...", id);
        let cid = ipfs_client.add_bytes(data).await?;
        
        // If pinning is enabled, pin the object
        if self.ipfs_pin {
            if let Err(e) = ipfs_client.pin(&cid).await {
                log::warn!("Failed to pin object {}: {}", id.as_str(), e);
            }
        }
        
        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.ipfs_object_count += 1;
            
            // If it's not stored locally, increment ipfs_only_count
            let object_path = self.get_object_path(id);
            if !object_path.exists() {
                stats.ipfs_only_count += 1;
            }
        }
        
        log::debug!("Stored LFS object {} in IPFS with CID {}", id, cid);
        
        Ok(cid)
    }
    
    /// Get the IPFS CID for an object
    pub async fn get_ipfs_cid(&self, id: &LfsObjectId) -> Option<String> {
        if let Some(metadata) = self.metadata_cache.read().await.get(id.as_str()) {
            return metadata.ipfs_cid.clone();
        }
        None
    }
    
    /// Set whether to automatically pin objects in IPFS
    pub fn set_ipfs_pin(&mut self, pin: bool) {
        self.ipfs_pin = pin;
    }
    
    /// Check whether IPFS integration is enabled
    pub fn has_ipfs(&self) -> bool {
        self.ipfs_client.is_some()
    }
    
    /// Get the IPFS client if available
    pub fn ipfs_client(&self) -> Option<&Arc<IpfsClient>> {
        self.ipfs_client.as_ref()
    }
    
    /// Import existing LFS objects from a Git repository
    pub async fn import_from_repo(&self, repo_path: impl AsRef<Path>) -> Result<u32> {
        let repo_path = repo_path.as_ref();
        let lfs_objects_path = repo_path.join(".git").join("lfs").join("objects");
        
        if !lfs_objects_path.exists() {
            return Ok(0);
        }
        
        log::info!("Importing LFS objects from repository: {}", repo_path.display());
        
        let mut count = 0;
        
        // Walk through the directory structure
        let mut dirs = tokio_fs::read_dir(&lfs_objects_path).await
            .map_err(|e| io_err(format!("Failed to read LFS objects directory: {}", e), &lfs_objects_path))?;
            
        while let Ok(Some(entry)) = dirs.next_entry().await {
            if !entry.file_type().await
                .map_err(|e| io_err(format!("Failed to get file type: {}", e), entry.path()))?.is_dir() {
                continue;
            }
            
            let prefix_dir = entry.path();
            let mut prefix_entries = tokio_fs::read_dir(&prefix_dir).await
                .map_err(|e| io_err(format!("Failed to read prefix directory: {}", e), &prefix_dir))?;
                
            while let Ok(Some(object_entry)) = prefix_entries.next_entry().await {
                if !object_entry.file_type().await
                    .map_err(|e| io_err(format!("Failed to get file type: {}", e), object_entry.path()))?.is_file() {
                    continue;
                }
                
                // Reconstruct the hash from the directory structure
                let prefix = prefix_dir.file_name()
                    .ok_or_else(|| GitError::LfsError("Invalid directory name".to_string()))?
                    .to_string_lossy();
                    
                let rest = object_entry.file_name().to_string_lossy();
                let hash = format!("{}{}", prefix, rest);
                
                // Create the object ID
                let id = LfsObjectId::new(&format!("sha256:{}", hash));
                
                // Read the file
                let data = tokio_fs::read(object_entry.path()).await
                    .map_err(|e| io_err(format!("Failed to read object file: {}", e), object_entry.path()))?;
                
                // Store it
                self.store_object(&id, &data).await?;
                count += 1;
                
                if count % 10 == 0 {
                    log::info!("Imported {} LFS objects so far...", count);
                }
            }
        }
        
        log::info!("Imported {} LFS objects from repository: {}", count, repo_path.display());
        
        Ok(count)
    }
    
    /// Clean up unused objects
    pub async fn gc(&self, keep_oids: &[LfsObjectId]) -> Result<u32> {
        log::info!("Starting LFS garbage collection...");
        
        let mut removed = 0;
        
        // Convert keep_oids to a HashSet for quick lookups
        let keep_set: std::collections::HashSet<_> = keep_oids.iter()
            .map(|id| id.as_str().to_string())
            .collect();
        
        // Walk through the base directory
        let mut dirs = tokio_fs::read_dir(self.base_dir.join("objects")).await
            .map_err(|e| io_err(format!("Failed to read objects directory: {}", e), self.base_dir.join("objects")))?;
            
        while let Ok(Some(entry)) = dirs.next_entry().await {
            if !entry.file_type().await
                .map_err(|e| io_err(format!("Failed to get file type: {}", e), entry.path()))?.is_dir() {
                continue;
            }
            
            let prefix_dir = entry.path();
            let prefix = prefix_dir.file_name()
                .ok_or_else(|| GitError::LfsError("Invalid directory name".to_string()))?
                .to_string_lossy();
                
            if prefix.len() != 2 {
                continue;
            }
            
            let mut prefix_entries = tokio_fs::read_dir(&prefix_dir).await
                .map_err(|e| io_err(format!("Failed to read prefix directory: {}", e), &prefix_dir))?;
                
            while let Ok(Some(object_entry)) = prefix_entries.next_entry().await {
                if !object_entry.file_type().await
                    .map_err(|e| io_err(format!("Failed to get file type: {}", e), object_entry.path()))?.is_file() {
                    continue;
                }
                
                // Reconstruct the hash from the directory structure
                let rest = object_entry.file_name().to_string_lossy();
                let hash = format!("{}{}", prefix, rest);
                let oid = format!("sha256:{}", hash);
                
                // If this object is not in the keep set, delete it
                if !keep_set.contains(&oid) {
                    log::debug!("Removing unused LFS object: {}", oid);
                    
                    tokio_fs::remove_file(object_entry.path()).await
                        .map_err(|e| io_err(format!("Failed to remove file: {}", e), object_entry.path()))?;
                    removed += 1;
                    
                    // Also unpin from IPFS if we have a cached CID
                    if let Some(ipfs_client) = &self.ipfs_client {
                        let id = LfsObjectId::new(&oid);
                        if let Some(cid) = self.get_ipfs_cid(&id).await {
                            if let Err(e) = ipfs_client.unpin(&cid).await {
                                log::warn!("Failed to unpin object {}: {}", oid, e);
                            }
                        }
                    }
                    
                    // Remove metadata
                    let metadata_path = self.get_metadata_path(&LfsObjectId::new(&oid));
                    if metadata_path.exists() {
                        if let Err(e) = tokio_fs::remove_file(&metadata_path).await {
                            log::warn!("Failed to remove metadata file: {}", e);
                        }
                    }
                    
                    // Remove from cache
                    self.metadata_cache.write().await.remove(&oid);
                }
            }
            
            // Check if the prefix directory is now empty
            let mut is_empty = true;
            let mut check_entries = tokio_fs::read_dir(&prefix_dir).await
                .map_err(|e| io_err(format!("Failed to read prefix directory: {}", e), &prefix_dir))?;
                
            if check_entries.next_entry().await
                .map_err(|e| io_err(format!("Failed to read directory entry: {}", e), &prefix_dir))?.is_some() {
                is_empty = false;
            }
            
            if is_empty {
                tokio_fs::remove_dir(&prefix_dir).await
                    .map_err(|e| io_err(format!("Failed to remove directory: {}", e), &prefix_dir))?;
            }
        }
        
        // Update stats after garbage collection
        await self.refresh_stats();
        
        log::info!("LFS garbage collection complete: Removed {} objects", removed);
        
        Ok(removed)
    }
    
    /// Refresh storage statistics
    async fn refresh_stats(&self) -> Result<LfsStorageStats> {
        let mut stats = LfsStorageStats::default();
        
        // Collect from metadata cache
        {
            let cache = self.metadata_cache.read().await;
            stats.object_count = cache.len();
            
            for metadata in cache.values() {
                stats.total_size += metadata.size;
                
                if metadata.ipfs_cid.is_some() {
                    stats.ipfs_object_count += 1;
                }
                
                // Check if the file exists locally
                let object_path = self.get_object_path(&LfsObjectId::new(&metadata.id));
                if object_path.exists() {
                    stats.local_object_count += 1;
                } else if metadata.ipfs_cid.is_some() {
                    // Object is in IPFS but not locally
                    stats.ipfs_only_count += 1;
                }
            }
        }
        
        // Update the stats
        *self.stats.write().await = stats.clone();
        
        Ok(stats)
    }
}

#[async_trait]
impl LfsObjectProvider for LfsStorage {
    async fn has_object(&self, id: &LfsObjectId) -> bool {
        // First, check the metadata cache
        if self.metadata_cache.read().await.contains_key(id.as_str()) {
            return true;
        }
        
        // Next, check the local filesystem
        let path = self.get_object_path(id);
        if path.exists() {
            return true;
        }
        
        // If IPFS is configured, check there as well
        if let Some(ipfs_client) = &self.ipfs_client {
            // Check if we have a cached CID for this object
            if let Some(cid) = self.get_ipfs_cid(id).await {
                match ipfs_client.exists(&cid).await {
                    Ok(exists) => return exists,
                    Err(_) => return false,
                }
            }
        }
        
        false
    }
    
    async fn get_object_bytes(&self, id: &LfsObjectId) -> Result<Bytes> {
        // Update cache hit/miss stats
        {
            let mut stats = self.stats.write().await;
            stats.cache_misses += 1;
        }
        
        // Determine where to get the object from based on storage priority
        if self.ipfs_primary && self.ipfs_client.is_some() {
            // Try IPFS first, then fall back to local storage
            if let Some(cid) = self.get_ipfs_cid(id).await {
                if let Ok(data) = self.ipfs_client.as_ref().unwrap().get_file(&cid).await {
                    // Update cache hit stats
                    {
                        let mut stats = self.stats.write().await;
                        stats.cache_hits += 1;
                        stats.cache_misses -= 1; // Undo the initial increment
                    }
                    
                    return Ok(data);
                }
            }
        }
        
        // Try local storage
        let path = self.get_object_path(id);
        if path.exists() {
            // Apply download throttling if configured
            let throttle = *self.download_throttle.read().await;
            if throttle > 0 {
                // Read file metadata to get size
                let metadata = tokio_fs::metadata(&path).await
                    .map_err(|e| io_err(format!("Failed to get file metadata: {}", e), &path))?;
                let file_size = metadata.len();
                
                // Calculate expected download time with throttling
                let expected_secs = (file_size as f64 / throttle as f64).ceil() as u64;
                
                if expected_secs > 0 {
                    // Open file
                    let mut file = tokio::fs::File::open(&path).await
                        .map_err(|e| io_err(format!("Failed to open file: {}", e), &path))?;
                    
                    // Calculate chunk size based on throttling
                    let chunk_size = (throttle / 10).max(1024) as usize; // At least 1KB chunks, aiming for 10 chunks per second
                    
                    // Create buffer to hold the entire file
                    let mut buffer = Vec::with_capacity(file_size as usize);
                    
                    loop {
                        let mut chunk = vec![0; chunk_size];
                        match tokio::io::AsyncReadExt::read(&mut file, &mut chunk).await {
                            Ok(0) => break, // EOF
                            Ok(n) => {
                                buffer.extend_from_slice(&chunk[..n]);
                                
                                // Throttle between chunks
                                let delay_ms = (1000.0 / 10.0) as u64; // Aim for 10 chunks per second
                                tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                            },
                            Err(e) => return Err(io_err(format!("Failed to read file: {}", e), &path)),
                        }
                    }
                    
                    // Update cache hit stats
                    {
                        let mut stats = self.stats.write().await;
                        stats.cache_hits += 1;
                        stats.cache_misses -= 1; // Undo the initial increment
                    }
                    
                    return Ok(Bytes::from(buffer));
                }
            }
            
            // No throttling or small file, read directly
            let data = tokio_fs::read(&path).await
                .map_err(|e| io_err(format!("Failed to read object file: {}", e), &path))?;
                
            // Update cache hit stats
            {
                let mut stats = self.stats.write().await;
                stats.cache_hits += 1;
                stats.cache_misses -= 1; // Undo the initial increment
            }
            
            return Ok(Bytes::from(data));
        }
        
        // If we're here and not IPFS-primary, try IPFS as a fallback
        if !self.ipfs_primary && self.ipfs_client.is_some() {
            if let Some(cid) = self.get_ipfs_cid(id).await {
                if let Ok(data) = self.ipfs_client.as_ref().unwrap().get_file(&cid).await {
                    // Cache the object locally for future use if we have metadata for size
                    if let Some(metadata) = self.metadata_cache.read().await.get(id.as_str()) {
                        // Only cache if we're not in ipfs-only mode and we have size information
                        if !self.ipfs_primary && metadata.size > 0 {
                            // Store in local cache asynchronously
                            let id_clone = LfsObjectId::new(id.as_str());
                            let data_clone = data.clone();
                            let self_clone = self.clone();
                            
                            tokio::spawn(async move {
                                if let Err(e) = self_clone.store_local(&id_clone, &data_clone).await {
                                    log::warn!("Failed to cache object locally: {}", e);
                                }
                            });
                        }
                    }
                    
                    // Update cache hit stats
                    {
                        let mut stats = self.stats.write().await;
                        stats.cache_hits += 1;
                        stats.cache_misses -= 1; // Undo the initial increment
                    }
                    
                    return Ok(data);
                }
            }
        }
        
        // Object not found in any storage
        Err(GitError::LfsError(format!("LFS object not found: {}", id.as_str())))
    }
    
    async fn store_object(&self, id: &LfsObjectId, data: &[u8]) -> Result<()> {
        log::debug!("Storing LFS object: {} ({} bytes)", id, data.len());
        
        // Get file metadata from pointer or extract from path
        let mut filename = None;
        let mut mimetype = None;
        
        // Store metadata
        let mut ipfs_cid = None;
        
        // Store in IPFS if configured
        if let Some(_) = &self.ipfs_client {
            match self.store_ipfs(id, data).await {
                Ok(cid) => {
                    ipfs_cid = Some(cid);
                },
                Err(e) => {
                    log::warn!("Failed to store object in IPFS: {}", e);
                }
            }
        }
        
        // Store metadata before storing the actual object
        self.save_metadata(id, data.len() as u64, ipfs_cid.clone(), filename, mimetype).await?;
        
        // Always store locally as well, unless explicitly configured not to
        if !self.ipfs_primary || self.ipfs_client.is_none() {
            self.store_local(id, data).await?;
        }
        
        // Update statistics
        {
            let mut stats = self.stats.write().await;
            stats.object_count += 1;
            stats.total_size += data.len() as u64;
            
            if ipfs_cid.is_some() {
                stats.ipfs_object_count += 1;
                
                if self.ipfs_primary {
                    stats.ipfs_only_count += 1;
                }
            }
        }
        
        log::debug!("LFS object stored successfully: {}", id);
        
        Ok(())
    }
    
    async fn delete_object(&self, id: &LfsObjectId) -> Result<()> {
        let path = self.get_object_path(id);
        
        log::debug!("Deleting LFS object: {}", id);
        
        // Get metadata for updating stats
        let metadata = self.metadata_cache.read().await.get(id.as_str()).cloned();
        
        // Delete from local storage if it exists
        if path.exists() {
            tokio_fs::remove_file(&path).await
                .map_err(|e| io_err(format!("Failed to delete object file: {}", e), &path))?;
                
            // Update stats
            {
                let mut stats = self.stats.write().await;
                stats.local_object_count -= 1;
            }
        }
        
        // Unpin from IPFS if we have a CID
        if let Some(ipfs_client) = &self.ipfs_client {
            if let Some(cid) = self.get_ipfs_cid(id).await {
                if let Err(e) = ipfs_client.unpin(&cid).await {
                    log::warn!("Failed to unpin object {}: {}", id.as_str(), e);
                } else {
                    // Update stats
                    {
                        let mut stats = self.stats.write().await;
                        stats.ipfs_object_count -= 1;
                        
                        if !path.exists() {
                            stats.ipfs_only_count -= 1;
                        }
                    }
                }
            }
        }
        
        // Delete metadata
        let metadata_path = self.get_metadata_path(id);
        if metadata_path.exists() {
            tokio_fs::remove_file(&metadata_path).await
                .map_err(|e| io_err(format!("Failed to delete metadata file: {}", e), &metadata_path))?;
        }
        
        // Remove from cache
        self.metadata_cache.write().await.remove(id.as_str());
        
        // Update stats
        if let Some(metadata) = metadata {
            let mut stats = self.stats.write().await;
            stats.object_count -= 1;
            stats.total_size -= metadata.size;
        }
        
        log::debug!("LFS object deleted: {}", id);
        
        Ok(())
    }
    
    async fn get_object_info(&self, id: &LfsObjectId) -> Result<LfsObjectInfo> {
        // Check metadata cache
        if let Some(metadata) = self.metadata_cache.read().await.get(id.as_str()) {
            let path = self.get_object_path(id);
            
            return Ok(LfsObjectInfo {
                id: metadata.id.clone(),
                size: metadata.size,
                ipfs_cid: metadata.ipfs_cid.clone(),
                is_local: path.exists(),
                filename: metadata.filename.clone(),
                mimetype: metadata.mimetype.clone(),
                added_at: metadata.added_at,
            });
        }
        
        // Check if the file exists locally but no metadata
        let path = self.get_object_path(id);
        if path.exists() {
            let metadata = tokio_fs::metadata(&path).await
                .map_err(|e| io_err(format!("Failed to get file metadata: {}", e), &path))?;
                
            // Create basic info
            return Ok(LfsObjectInfo {
                id: id.as_str().to_string(),
                size: metadata.len(),
                ipfs_cid: None,
                is_local: true,
                filename: None,
                mimetype: None,
                added_at: chrono::DateTime::from(metadata.modified()
                    .map_err(|e| io_err(format!("Failed to get modification time: {}", e), &path))?)
                    .into(),
            });
        }
        
        // Check IPFS as a last resort
        if let Some(ipfs_client) = &self.ipfs_client {
            if let Some(cid) = self.get_ipfs_cid(id).await {
                if ipfs_client.exists(&cid).await.unwrap_or(false) {
                    // We have it in IPFS but can't get size without downloading, so use 0
                    return Ok(LfsObjectInfo {
                        id: id.as_str().to_string(),
                        size: 0, // Unknown without downloading
                        ipfs_cid: Some(cid),
                        is_local: false,
                        filename: None,
                        mimetype: None,
                        added_at: chrono::Utc::now(),
                    });
                }
            }
        }
        
        Err(GitError::LfsError(format!("LFS object not found: {}", id.as_str())))
    }
    
    async fn get_stats(&self) -> Result<LfsStorageStats> {
        // Just return a clone of the current stats
        Ok(*self.stats.read().await)
    }
}

impl Clone for LfsStorage {
    fn clone(&self) -> Self {
        Self {
            base_dir: self.base_dir.clone(),
            ipfs_client: self.ipfs_client.clone(),
            ipfs_primary: self.ipfs_primary,
            ipfs_pin: self.ipfs_pin,
            metadata_cache: RwLock::new(self.metadata_cache.try_read().unwrap_or_default().clone()),
            stats: RwLock::new(*self.stats.try_read().unwrap_or(&LfsStorageStats::default())),
            upload_throttle: RwLock::new(*self.upload_throttle.try_read().unwrap_or(&0)),
            download_throttle: RwLock::new(*self.download_throttle.try_read().unwrap_or(&0)),
        }
    }
}