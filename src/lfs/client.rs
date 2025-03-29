use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::io::Read;
use bytes::Bytes;
use sha2::{Sha256, Digest};
use glob::Pattern;
use reqwest::Client as HttpClient;

use crate::core::{GitError, Result};
use super::{LfsConfig, LfsPointer, LfsObjectId, LfsStorage};

/// Client for Git LFS operations
pub struct LfsClient {
    /// LFS configuration
    config: LfsConfig,
    
    /// Storage for LFS objects
    storage: Arc<LfsStorage>,
    
    /// HTTP client for API requests
    http: HttpClient,
}

impl LfsClient {
    /// Create a new LFS client with the given configuration and storage
    pub fn new(config: LfsConfig, storage: Arc<LfsStorage>) -> Self {
        Self {
            config,
            storage,
            http: HttpClient::new(),
        }
    }
    
    /// Get the LFS configuration
    pub fn config(&self) -> &LfsConfig {
        &self.config
    }
    
    /// Get a mutable reference to the LFS configuration
    pub fn config_mut(&mut self) -> &mut LfsConfig {
        &mut self.config
    }
    
    /// Check if a file should be tracked by LFS based on its path and optional size
    pub fn should_track(&self, path: &Path, size: Option<u64>) -> bool {
        // If LFS is not enabled, don't track anything
        if !self.config.enabled {
            return false;
        }
        
        // Check size threshold if available
        if let Some(file_size) = size {
            if file_size >= self.config.size_threshold {
                return true;
            }
        }
        
        // Check file patterns
        let file_name = match path.file_name() {
            Some(name) => name.to_string_lossy().to_string(),
            None => return false,
        };
        
        for pattern_str in &self.config.track_patterns {
            match Pattern::new(pattern_str) {
                Ok(pattern) => {
                    if pattern.matches(&file_name) {
                        return true;
                    }
                },
                Err(_) => continue,
            }
        }
        
        false
    }
    
    /// Store a file in LFS storage and return a pointer to it
    pub async fn store_file(&self, path: impl AsRef<Path>) -> Result<LfsPointer> {
        let path = path.as_ref();
        
        // Read the file
        let mut file = std::fs::File::open(path)
            .map_err(|e| GitError::IO(format!("Failed to open file for LFS: {}", e)))?;
            
        // Get the file size
        let metadata = file.metadata()
            .map_err(|e| GitError::IO(format!("Failed to get file metadata: {}", e)))?;
        let size = metadata.len();
        
        // Calculate the SHA256 hash of the file
        let mut hasher = Sha256::new();
        let mut buffer = Vec::with_capacity(size as usize);
        file.read_to_end(&mut buffer)
            .map_err(|e| GitError::IO(format!("Failed to read file: {}", e)))?;
            
        hasher.update(&buffer);
        let hash = format!("{:x}", hasher.finalize());
        
        // Create the object ID and pointer
        let oid_str = format!("sha256:{}", hash);
        let id = LfsObjectId::new(&oid_str);
        let mut pointer = LfsPointer::new(&oid_str, size);
        
        // Store the object
        self.storage.store_object(&id, &buffer).await?;
        
        // If IPFS is enabled, store it there too
        if self.config.use_ipfs {
            // TODO: Upload to IPFS and get CID
            if let Ok(cid) = self.upload_to_ipfs(&buffer).await {
                pointer.set_ipfs_cid(&cid);
            }
        }
        
        Ok(pointer)
    }
    
    /// Get an LFS object and store it to a file
    pub async fn get_object(&self, pointer: &LfsPointer, dest_path: impl AsRef<Path>) -> Result<()> {
        let dest_path = dest_path.as_ref();
        let id = LfsObjectId::new(&pointer.oid);
        
        // Try to get the object from local storage first
        if let Ok(data) = self.storage.get_object_bytes(&id).await {
            // Write the object to the destination path
            std::fs::write(dest_path, data)
                .map_err(|e| GitError::IO(format!("Failed to write LFS object to file: {}", e)))?;
                
            return Ok(());
        }
        
        // If IPFS is enabled and we have a CID, try to get it from IPFS
        if self.config.use_ipfs && pointer.ipfs_cid.is_some() {
            if let Some(cid) = &pointer.ipfs_cid {
                if let Ok(data) = self.fetch_from_ipfs(cid).await {
                    std::fs::write(dest_path, data)
                        .map_err(|e| GitError::IO(format!("Failed to write LFS object from IPFS: {}", e)))?;
                        
                    return Ok(());
                }
            }
        }
        
        // Object not found either locally or in IPFS
        Err(GitError::LfsError(format!("LFS object not found: {}", pointer.oid)))
    }
    
    /// Upload an object to IPFS and return its CID
    pub async fn upload_to_ipfs(&self, data: &[u8]) -> Result<String> {
        // Check if IPFS is enabled and gateway is configured
        if !self.config.use_ipfs || self.config.ipfs_gateway.is_none() {
            return Err(GitError::LfsError("IPFS not configured".to_string()));
        }
        
        let gateway = self.config.ipfs_gateway.as_ref().unwrap();
        
        // This is a simplified version using the HTTP API
        // In a real implementation, you would use a proper IPFS client library
        
        // Format the API endpoint URL
        let url = format!("{}/api/v0/add", gateway);
        
        // Create a form with the file data
        let form = reqwest::multipart::Form::new()
            .part("file", reqwest::multipart::Part::bytes(data.to_vec()));
        
        // Make the POST request
        let response = self.http.post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| GitError::External(format!("Failed to upload to IPFS: {}", e)))?;
            
        // Parse the response
        let result = response.json::<serde_json::Value>().await
            .map_err(|e| GitError::External(format!("Failed to parse IPFS response: {}", e)))?;
            
        // Extract the CID (hash) from the response
        if let Some(cid) = result["Hash"].as_str() {
            Ok(cid.to_string())
        } else {
            Err(GitError::External("Failed to get CID from IPFS response".to_string()))
        }
    }
    
    /// Fetch an object from IPFS by CID
    pub async fn fetch_from_ipfs(&self, cid: &str) -> Result<Bytes> {
        // Check if IPFS is enabled and gateway is configured
        if !self.config.use_ipfs || self.config.ipfs_gateway.is_none() {
            return Err(GitError::LfsError("IPFS not configured".to_string()));
        }
        
        let gateway = self.config.ipfs_gateway.as_ref().unwrap();
        
        // Format the gateway URL to fetch the object
        let url = format!("{}/ipfs/{}", gateway, cid);
        
        // Fetch the content
        let response = self.http.get(&url)
            .send()
            .await
            .map_err(|e| GitError::External(format!("Failed to fetch from IPFS: {}", e)))?;
            
        // Check if the request was successful
        if !response.status().is_success() {
            return Err(GitError::External(format!("IPFS gateway returned error: {}", response.status())));
        }
        
        // Get the bytes
        let bytes = response.bytes()
            .await
            .map_err(|e| GitError::External(format!("Failed to read IPFS response: {}", e)))?;
            
        Ok(bytes)
    }
}