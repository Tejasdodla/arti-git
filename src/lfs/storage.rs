/// LFS object storage implementation
use std::path::{Path, PathBuf};
use std::fs;
use std::io;
use std::sync::Arc;
use async_trait::async_trait;
use tokio::fs as tokio_fs;
use bytes::Bytes;

use crate::core::{GitError, Result};
use crate::ipfs::IpfsClient;

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
}

impl From<io::Error> for LfsStorageError {
    fn from(err: io::Error) -> Self {
        LfsStorageError::Io(err)
    }
}

impl From<LfsStorageError> for GitError {
    fn from(err: LfsStorageError) -> Self {
        match err {
            LfsStorageError::Io(e) => GitError::IO(e.to_string()),
            LfsStorageError::NotFound(msg) => GitError::LfsError(msg),
            LfsStorageError::InvalidId(msg) => GitError::LfsError(msg),
            LfsStorageError::IpfsError(msg) => GitError::IpfsError(msg),
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
    
    /// Cache of IPFS CIDs for objects (oid -> CID)
    cid_cache: parking_lot::RwLock<std::collections::HashMap<String, String>>,
}

impl LfsStorage {
    /// Create a new LFS storage with the given base directory
    pub fn new(base_dir: impl AsRef<Path>) -> Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();
        
        // Ensure the directory exists
        fs::create_dir_all(&base_dir)
            .map_err(|e| GitError::LfsError(format!("Failed to create LFS storage directory: {}", e)))?;
        
        Ok(Self {
            base_dir,
            ipfs_client: None,
            ipfs_primary: false,
            ipfs_pin: true,
            cid_cache: parking_lot::RwLock::new(std::collections::HashMap::new()),
        })
    }
    
    /// Create a new LFS storage with IPFS integration
    pub fn with_ipfs(base_dir: impl AsRef<Path>, ipfs_client: Arc<IpfsClient>, ipfs_primary: bool) -> Result<Self> {
        let mut storage = Self::new(base_dir)?;
        storage.ipfs_client = Some(ipfs_client);
        storage.ipfs_primary = ipfs_primary;
        Ok(storage)
    }
    
    /// Get the path for an object
    fn get_object_path(&self, id: &LfsObjectId) -> PathBuf {
        let hash = id.hash();
        
        // Use the first 2 characters as a directory prefix for better file distribution
        let prefix = &hash[0..2];
        let rest = &hash[2..];
        
        self.base_dir.join(prefix).join(rest)
    }
    
    /// Store an object in the local filesystem
    async fn store_local(&self, id: &LfsObjectId, data: &[u8]) -> Result<()> {
        let path = self.get_object_path(id);
        
        // Ensure the parent directory exists
        if let Some(parent) = path.parent() {
            tokio_fs::create_dir_all(parent).await
                .map_err(|e| GitError::LfsError(format!("Failed to create directory: {}", e)))?;
        }
        
        // Write the file
        tokio_fs::write(&path, data).await
            .map_err(|e| GitError::LfsError(format!("Failed to write object file: {}", e)))?;
        
        Ok(())
    }
    
    /// Store an object in IPFS
    async fn store_ipfs(&self, id: &LfsObjectId, data: &[u8]) -> Result<String> {
        let ipfs_client = self.ipfs_client.as_ref()
            .ok_or_else(|| GitError::LfsError("IPFS client not configured".to_string()))?;
        
        // Upload to IPFS
        let cid = ipfs_client.add_bytes(data).await?;
        
        // If pinning is enabled, pin the object
        if self.ipfs_pin {
            if let Err(e) = ipfs_client.pin(&cid).await {
                eprintln!("Warning: Failed to pin object {}: {}", id.as_str(), e);
            }
        }
        
        // Cache the CID
        self.cid_cache.write().insert(id.as_str().to_string(), cid.clone());
        
        Ok(cid)
    }
    
    /// Get the IPFS CID for an object
    pub fn get_ipfs_cid(&self, id: &LfsObjectId) -> Option<String> {
        self.cid_cache.read().get(id.as_str()).cloned()
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
        
        let mut count = 0;
        
        // Walk through the directory structure
        let mut dirs = tokio_fs::read_dir(&lfs_objects_path).await
            .map_err(|e| GitError::LfsError(format!("Failed to read LFS objects directory: {}", e)))?;
            
        while let Ok(Some(entry)) = dirs.next_entry().await {
            if !entry.file_type().await
                .map_err(|e| GitError::LfsError(format!("Failed to get file type: {}", e)))?.is_dir() {
                continue;
            }
            
            let prefix_dir = entry.path();
            let mut prefix_entries = tokio_fs::read_dir(&prefix_dir).await
                .map_err(|e| GitError::LfsError(format!("Failed to read prefix directory: {}", e)))?;
                
            while let Ok(Some(object_entry)) = prefix_entries.next_entry().await {
                if !object_entry.file_type().await
                    .map_err(|e| GitError::LfsError(format!("Failed to get file type: {}", e)))?.is_file() {
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
                    .map_err(|e| GitError::LfsError(format!("Failed to read object file: {}", e)))?;
                
                // Store it
                self.store_object(&id, &data).await?;
                count += 1;
            }
        }
        
        Ok(count)
    }
    
    /// Clean up unused objects
    pub async fn gc(&self, keep_oids: &[LfsObjectId]) -> Result<u32> {
        let mut removed = 0;
        
        // Convert keep_oids to a HashSet for quick lookups
        let keep_set: std::collections::HashSet<_> = keep_oids.iter()
            .map(|id| id.as_str().to_string())
            .collect();
        
        // Walk through the base directory
        let mut dirs = tokio_fs::read_dir(&self.base_dir).await
            .map_err(|e| GitError::LfsError(format!("Failed to read base directory: {}", e)))?;
            
        while let Ok(Some(entry)) = dirs.next_entry().await {
            if !entry.file_type().await
                .map_err(|e| GitError::LfsError(format!("Failed to get file type: {}", e)))?.is_dir() {
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
                .map_err(|e| GitError::LfsError(format!("Failed to read prefix directory: {}", e)))?;
                
            while let Ok(Some(object_entry)) = prefix_entries.next_entry().await {
                if !object_entry.file_type().await
                    .map_err(|e| GitError::LfsError(format!("Failed to get file type: {}", e)))?.is_file() {
                    continue;
                }
                
                // Reconstruct the hash from the directory structure
                let rest = object_entry.file_name().to_string_lossy();
                let hash = format!("{}{}", prefix, rest);
                let oid = format!("sha256:{}", hash);
                
                // If this object is not in the keep set, delete it
                if !keep_set.contains(&oid) {
                    tokio_fs::remove_file(object_entry.path()).await
                        .map_err(|e| GitError::LfsError(format!("Failed to remove file: {}", e)))?;
                    removed += 1;
                    
                    // Also unpin from IPFS if we have a cached CID
                    if let Some(ipfs_client) = &self.ipfs_client {
                        if let Some(cid) = self.get_ipfs_cid(&LfsObjectId::new(&oid)) {
                            if let Err(e) = ipfs_client.unpin(&cid).await {
                                eprintln!("Warning: Failed to unpin object {}: {}", oid, e);
                            } else {
                                // Remove from cache - Fix: use the string value as key, not the reference
                                self.cid_cache.write().remove(oid.as_str());
                            }
                        }
                    }
                }
            }
            
            // Check if the prefix directory is now empty
            let mut is_empty = true;
            let mut check_entries = tokio_fs::read_dir(&prefix_dir).await
                .map_err(|e| GitError::LfsError(format!("Failed to read prefix directory: {}", e)))?;
                
            if check_entries.next_entry().await
                .map_err(|e| GitError::LfsError(format!("Failed to read directory entry: {}", e)))?.is_some() {
                is_empty = false;
            }
            
            if is_empty {
                tokio_fs::remove_dir(&prefix_dir).await
                    .map_err(|e| GitError::LfsError(format!("Failed to remove directory: {}", e)))?;
            }
        }
        
        Ok(removed)
    }
}

#[async_trait]
impl LfsObjectProvider for LfsStorage {
    async fn has_object(&self, id: &LfsObjectId) -> bool {
        // First, check the local filesystem
        let path = self.get_object_path(id);
        if path.exists() {
            return true;
        }
        
        // If IPFS is configured, check there as well
        if let Some(ipfs_client) = &self.ipfs_client {
            // Check if we have a cached CID for this object
            if let Some(cid) = self.get_ipfs_cid(id) {
                match ipfs_client.exists(&cid).await {
                    Ok(exists) => return exists,
                    Err(_) => return false,
                }
            }
        }
        
        false
    }
    
    async fn get_object_bytes(&self, id: &LfsObjectId) -> Result<Bytes> {
        // Determine where to get the object from based on storage priority
        if self.ipfs_primary && self.ipfs_client.is_some() {
            // Try IPFS first, then fall back to local storage
            if let Some(cid) = self.get_ipfs_cid(id) {
                if let Ok(data) = self.ipfs_client.as_ref().unwrap().get_file(&cid).await {
                    return Ok(data);
                }
            }
        }
        
        // Try local storage
        let path = self.get_object_path(id);
        if path.exists() {
            let data = tokio_fs::read(&path).await
                .map_err(|e| GitError::LfsError(format!("Failed to read object file: {}", e)))?;
                
            return Ok(Bytes::from(data));
        }
        
        // If we're here and not IPFS-primary, try IPFS as a fallback
        if !self.ipfs_primary && self.ipfs_client.is_some() {
            if let Some(cid) = self.get_ipfs_cid(id) {
                if let Ok(data) = self.ipfs_client.as_ref().unwrap().get_file(&cid).await {
                    // Cache the object locally for future use
                    self.store_local(id, &data).await?;
                    return Ok(data);
                }
            }
        }
        
        // Object not found in any storage
        Err(GitError::LfsError(format!("LFS object not found: {}", id.as_str())))
    }
    
    async fn store_object(&self, id: &LfsObjectId, data: &[u8]) -> Result<()> {
        // Store in IPFS if configured
        if let Some(_) = &self.ipfs_client {
            if let Ok(cid) = self.store_ipfs(id, data).await {
                println!("Stored LFS object {} in IPFS with CID: {}", id.as_str(), cid);
            }
        }
        
        // Always store locally as well, unless explicitly configured not to
        if !self.ipfs_primary || self.ipfs_client.is_none() {
            self.store_local(id, data).await?;
        }
        
        Ok(())
    }
    
    async fn delete_object(&self, id: &LfsObjectId) -> Result<()> {
        let path = self.get_object_path(id);
        
        // Delete from local storage if it exists
        if path.exists() {
            tokio_fs::remove_file(&path).await
                .map_err(|e| GitError::LfsError(format!("Failed to delete object file: {}", e)))?;
        }
        
        // Unpin from IPFS if we have a CID
        if let Some(ipfs_client) = &self.ipfs_client {
            if let Some(cid) = self.get_ipfs_cid(id) {
                if let Err(e) = ipfs_client.unpin(&cid).await {
                    eprintln!("Warning: Failed to unpin object {}: {}", id.as_str(), e);
                } else {
                    // Remove from cache
                    self.cid_cache.write().remove(id.as_str());
                }
            }
        }
        
        Ok(())
    }
}