use std::sync::Arc;
use std::path::Path;
use bytes::Bytes;
use ipfs_api_backend_hyper::{IpfsApi, IpfsClient as ApiClient, TryFromUri};
use url::Url;

use crate::core::{GitError, Result};
use super::config::IpfsConfig;

/// Client for interacting with IPFS
pub struct IpfsClient {
    /// The underlying IPFS API client
    api: ApiClient,
    
    /// IPFS configuration
    config: IpfsConfig,
}

impl IpfsClient {
    /// Create a new IPFS client with the given configuration
    pub async fn new(config: IpfsConfig) -> Result<Self> {
        if !config.enabled {
            return Err(GitError::Config("IPFS integration is not enabled".to_string()));
        }
        
        // Create API URL for the IPFS client
        let api_url = format!("{}:{}", config.api_endpoint, config.api_port);
        
        // Create the client
        let api = ApiClient::from_str(&api_url)
            .map_err(|e| GitError::Transport(format!("Failed to create IPFS client: {}", e)))?;
            
        // Check connectivity to make sure the daemon is running
        if config.use_local_daemon {
            match api.version().await {
                Ok(version) => {
                    println!("Connected to IPFS daemon version: {}", version.version);
                },
                Err(e) => {
                    if config.start_daemon_if_needed {
                        // TODO: Implement starting the IPFS daemon
                        // This would involve spawning a process to start the daemon
                        return Err(GitError::Transport(format!("IPFS daemon not running and auto-start not implemented yet: {}", e)));
                    } else {
                        return Err(GitError::Transport(format!("Failed to connect to IPFS daemon: {}", e)));
                    }
                }
            }
        }
        
        Ok(Self {
            api,
            config,
        })
    }
    
    /// Add a file to IPFS
    pub async fn add_file(&self, path: impl AsRef<Path>) -> Result<String> {
        let path = path.as_ref();
        
        // Read the file
        let file_data = tokio::fs::read(path)
            .await
            .map_err(|e| GitError::IO(format!("Failed to read file {}: {}", path.display(), e)))?;
            
        // Add to IPFS
        let response = self.api.add(file_data)
            .await
            .map_err(|e| GitError::Transport(format!("Failed to add file to IPFS: {}", e)))?;
            
        // Pin the object if configured
        if self.config.pin_objects {
            self.api.pin_add(&response.hash, true)
                .await
                .map_err(|e| GitError::Transport(format!("Failed to pin object {}: {}", response.hash, e)))?;
        }
        
        Ok(response.hash)
    }
    
    /// Add raw bytes to IPFS
    pub async fn add_bytes(&self, data: &[u8]) -> Result<String> {
        // Add to IPFS
        let response = self.api.add(data.to_vec())
            .await
            .map_err(|e| GitError::Transport(format!("Failed to add data to IPFS: {}", e)))?;
            
        // Pin the object if configured
        if self.config.pin_objects {
            self.api.pin_add(&response.hash, true)
                .await
                .map_err(|e| GitError::Transport(format!("Failed to pin object {}: {}", response.hash, e)))?;
        }
        
        Ok(response.hash)
    }
    
    /// Get a file from IPFS by its hash
    pub async fn get_file(&self, hash: &str) -> Result<Bytes> {
        let data = self.api.cat(hash)
            .await
            .map_err(|e| GitError::Transport(format!("Failed to get file from IPFS: {}", e)))?;
            
        Ok(Bytes::from(data))
    }
    
    /// Pin an object in IPFS
    pub async fn pin(&self, hash: &str) -> Result<()> {
        self.api.pin_add(hash, true)
            .await
            .map_err(|e| GitError::Transport(format!("Failed to pin object {}: {}", hash, e)))?;
            
        Ok(())
    }
    
    /// Unpin an object from IPFS
    pub async fn unpin(&self, hash: &str) -> Result<()> {
        self.api.pin_rm(hash, true)
            .await
            .map_err(|e| GitError::Transport(format!("Failed to unpin object {}: {}", hash, e)))?;
            
        Ok(())
    }
    
    /// Get information about the IPFS node
    pub async fn node_info(&self) -> Result<String> {
        let id = self.api.id(None)
            .await
            .map_err(|e| GitError::Transport(format!("Failed to get node ID: {}", e)))?;
            
        Ok(format!(
            "ID: {}\nPublic Key: {}\nAddresses: {}\nAgent Version: {}\nProtocols: {}",
            id.id,
            id.public_key,
            id.addresses.join(", "),
            id.agent_version,
            id.protocols.join(", ")
        ))
    }
    
    /// Check if an object exists in IPFS
    pub async fn exists(&self, hash: &str) -> bool {
        match self.api.block_stat(hash).await {
            Ok(_) => true,
            Err(_) => false,
        }
    }
    
    /// Get a reference to the configuration
    pub fn config(&self) -> &IpfsConfig {
        &self.config
    }
}