use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::io::{self, Read, Write};
use std::fs::{self, File};
use bytes::Bytes;
use async_trait::async_trait;

use crate::core::{GitError, Result};
use crate::ipfs::IpfsClient;
use super::{LfsConfig, LfsPointer};

/// Represents an LFS object identifier
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LfsObjectId {
    /// The SHA-256 hash of the object
    pub hash: String,
}

impl LfsObjectId {
    /// Create a new LFS object ID from a hash
    pub fn new(hash: &str) -> Self {
        Self {
            hash: hash.to_string(),
        }
    }
    
    /// Create an LFS object ID from a pointer
    pub fn from_pointer(pointer: &LfsPointer) -> Self {
        Self::new(pointer.hash())
    }
    
    /// Get the first two characters of the hash (used for directory partitioning)
    pub fn prefix(&self) -> &str {
        &self.hash[0..2]
    }
    
    /// Get the remaining characters of the hash (used as the filename)
    pub fn suffix(&self) -> &str {
        &self.hash[2..]
    }
}

/// Storage for LFS objects, integrating with IPFS
pub struct LfsStorage {
    /// Configuration for LFS operations
    config: LfsConfig,
    
    /// IPFS client for storing and retrieving LFS objects
    ipfs_client: Option<Arc<IpfsClient>>,
}

impl LfsStorage {
    /// Create a new LFS storage with the given configuration
    pub fn new(config: LfsConfig, ipfs_client: Option<Arc<IpfsClient>>) -> Self {
        Self {
            config,
            ipfs_client,
        }
    }
    
    /// Get the path to an object in local storage
    pub fn get_object_path(&self, id: &LfsObjectId) -> PathBuf {
        let mut path = self.config.objects_dir.clone();
        path.push(id.prefix());
        path.push(id.suffix());
        path
    }
    
    /// Check if an object exists in local storage
    pub fn has_object_locally(&self, id: &LfsObjectId) -> bool {
        self.get_object_path(id).exists()
    }
    
    /// Store an object in local storage
    pub fn store_locally(&self, id: &LfsObjectId, data: &[u8]) -> Result<()> {
        let obj_path = self.get_object_path(id);
        
        // Create parent directory if it doesn't exist
        if let Some(parent) = obj_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| GitError::IO(format!("Failed to create LFS object directory: {}", e)))?;
        }
        
        // Write data to file
        let mut file = File::create(&obj_path)
            .map_err(|e| GitError::IO(format!("Failed to create LFS object file: {}", e)))?;
            
        file.write_all(data)
            .map_err(|e| GitError::IO(format!("Failed to write LFS object data: {}", e)))?;
            
        Ok(())
    }
    
    /// Get an object from local storage
    pub fn get_locally(&self, id: &LfsObjectId) -> Result<Bytes> {
        let obj_path = self.get_object_path(id);
        
        if !obj_path.exists() {
            return Err(GitError::LfsError(format!("LFS object not found locally: {}", id.hash)));
        }
        
        let mut file = File::open(&obj_path)
            .map_err(|e| GitError::IO(format!("Failed to open LFS object: {}", e)))?;
            
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)
            .map_err(|e| GitError::IO(format!("Failed to read LFS object: {}", e)))?;
            
        Ok(Bytes::from(buffer))
    }
    
    /// Store an object in IPFS
    pub async fn store_in_ipfs(&self, id: &LfsObjectId, data: &[u8]) -> Result<String> {
        if !self.config.use_ipfs || self.ipfs_client.is_none() {
            return Err(GitError::Config("IPFS is not enabled or configured".to_string()));
        }
        
        if let Some(ipfs) = &self.ipfs_client {
            let cid = ipfs.add_bytes(data).await?;
            Ok(cid)
        } else {
            Err(GitError::Config("IPFS client not available".to_string()))
        }
    }
    
    /// Get an object from IPFS
    pub async fn get_from_ipfs(&self, cid: &str) -> Result<Bytes> {
        if !self.config.use_ipfs || self.ipfs_client.is_none() {
            return Err(GitError::Config("IPFS is not enabled or configured".to_string()));
        }
        
        if let Some(ipfs) = &self.ipfs_client {
            let data = ipfs.get_file(cid).await?;
            Ok(data)
        } else {
            Err(GitError::Config("IPFS client not available".to_string()))
        }
    }
    
    /// Store an object in both local storage and IPFS
    pub async fn store_object(&self, id: &LfsObjectId, data: &[u8]) -> Result<Option<String>> {
        // Always store locally
        self.store_locally(id, data)?;
        
        // If IPFS is enabled, store there too
        let cid = if self.config.use_ipfs && self.ipfs_client.is_some() {
            Some(self.store_in_ipfs(id, data).await?)
        } else {
            None
        };
        
        Ok(cid)
    }
    
    /// Get an object, trying local storage first then IPFS if available
    pub async fn get_object(&self, id: &LfsObjectId, ipfs_cid: Option<&str>) -> Result<Bytes> {
        // Try to get locally first
        if self.has_object_locally(id) {
            return self.get_locally(id);
        }
        
        // If not found locally, try IPFS if enabled and CID is provided
        if self.config.use_ipfs && self.ipfs_client.is_some() {
            if let Some(cid) = ipfs_cid {
                let data = self.get_from_ipfs(cid).await?;
                
                // Store in local cache for future use
                self.store_locally(id, &data)?;
                
                return Ok(data);
            }
        }
        
        // If we got here, the object isn't available
        Err(GitError::LfsError(format!("LFS object not found: {}", id.hash)))
    }
}