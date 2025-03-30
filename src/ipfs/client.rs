/// IPFS client for ArtiGit
///
/// This module provides a client for interacting with IPFS
use std::path::{Path, PathBuf};
use bytes::Bytes;
use reqwest::Client as HttpClient;
use reqwest::multipart;
use serde::Deserialize;
use serde_json::Value;

use crate::core::{GitError, Result};
use super::IpfsConfig;

/// Client for interacting with IPFS nodes
pub struct IpfsClient {
    /// Configuration for IPFS
    config: IpfsConfig,
    
    /// HTTP client for API calls
    http: HttpClient,
}

/// Response from the IPFS add operation
#[derive(Debug, Deserialize)]
struct AddResponse {
    /// The IPFS hash (CID) of the added content
    #[serde(rename = "Hash")]
    hash: String,
    
    /// The name of the added file
    #[serde(rename = "Name")]
    name: String,
    
    /// The size of the added content
    #[serde(rename = "Size")]
    size: String,
}

/// Response from the IPFS pin operation
#[derive(Debug, Deserialize)]
struct PinResponse {
    /// The pins that were created
    #[serde(rename = "Pins")]
    pins: Vec<String>,
}

impl IpfsClient {
    /// Create a new IPFS client
    pub async fn new(config: IpfsConfig) -> Result<Self> {
        // Create HTTP client
        let http = HttpClient::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| GitError::IpfsError(format!("Failed to create HTTP client: {}", e)))?;
            
        // Create client
        let client = Self {
            config,
            http,
        };
        
        // Check if the IPFS node is available
        client.is_available().await?;
        
        Ok(client)
    }
    
    /// Check if the IPFS node is available
    pub async fn is_available(&self) -> Result<bool> {
        let url = format!("{}/api/v0/id", self.config.api_url);
        
        match self.http.post(&url).send().await {
            Ok(response) => {
                if response.status().is_success() {
                    Ok(true)
                } else {
                    Err(GitError::IpfsError(format!("IPFS node returned error: {}", response.status())))
                }
            }
            Err(e) => Err(GitError::IpfsError(format!("Failed to connect to IPFS node: {}", e))),
        }
    }
    
    /// Add a file to IPFS
    pub async fn add_file(&self, path: impl AsRef<Path>) -> Result<String> {
        let path = path.as_ref();
        
        // Ensure the file exists
        if !path.exists() {
            return Err(GitError::IpfsError(format!("File does not exist: {}", path.display())));
        }
        
        // Create the form with the file
        let file_data = tokio::fs::read(path).await
            .map_err(|e| GitError::IpfsError(format!("Failed to read file: {}", e)))?;
            
        let file_name = path.file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
            
        // Build the form with the file data
        let form = multipart::Form::new()
            .part("file", multipart::Part::bytes(file_data).file_name(file_name));
            
        // Make the API request
        let url = format!("{}/api/v0/add", self.config.api_url);
        
        let response = self.http.post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| GitError::IpfsError(format!("Failed to upload to IPFS: {}", e)))?;
            
        if !response.status().is_success() {
            let error = response.text().await
                .unwrap_or_else(|_| "Unknown error".to_string());
                
            return Err(GitError::IpfsError(format!("IPFS add failed: {}", error)));
        }
        
        // Parse the response
        let add_response: AddResponse = response.json().await
            .map_err(|e| GitError::IpfsError(format!("Failed to parse IPFS response: {}", e)))?;
            
        // If configured to pin automatically, pin this object
        if self.config.auto_pin {
            if let Err(e) = self.pin(&add_response.hash).await {
                eprintln!("Warning: Failed to pin IPFS object: {}", e);
            }
        }
            
        Ok(add_response.hash)
    }
    
    /// Add raw bytes to IPFS
    pub async fn add_bytes(&self, data: &[u8]) -> Result<String> {
        // Build the form with the data
        let form = multipart::Form::new()
            .part("file", multipart::Part::bytes(data.to_vec()).file_name("data"));
            
        // Make the API request
        let url = format!("{}/api/v0/add", self.config.api_url);
        
        let response = self.http.post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| GitError::IpfsError(format!("Failed to upload to IPFS: {}", e)))?;
            
        if !response.status().is_success() {
            let error = response.text().await
                .unwrap_or_else(|_| "Unknown error".to_string());
                
            return Err(GitError::IpfsError(format!("IPFS add failed: {}", error)));
        }
        
        // Parse the response
        let add_response: AddResponse = response.json().await
            .map_err(|e| GitError::IpfsError(format!("Failed to parse IPFS response: {}", e)))?;
            
        // If configured to pin automatically, pin this object
        if self.config.auto_pin {
            if let Err(e) = self.pin(&add_response.hash).await {
                eprintln!("Warning: Failed to pin IPFS object: {}", e);
            }
        }
            
        Ok(add_response.hash)
    }
    
    /// Get a file from IPFS by CID
    pub async fn get_file(&self, cid: &str) -> Result<Bytes> {
        let url = format!("{}/api/v0/cat?arg={}", self.config.api_url, cid);
        
        let response = self.http.post(&url)
            .send()
            .await
            .map_err(|e| GitError::IpfsError(format!("Failed to get file from IPFS: {}", e)))?;
            
        if !response.status().is_success() {
            let error = response.text().await
                .unwrap_or_else(|_| "Unknown error".to_string());
                
            return Err(GitError::IpfsError(format!("IPFS cat failed: {}", error)));
        }
        
        let bytes = response.bytes().await
            .map_err(|e| GitError::IpfsError(format!("Failed to read response body: {}", e)))?;
            
        Ok(bytes)
    }
    
    /// Check if a file exists in IPFS
    pub async fn exists(&self, cid: &str) -> Result<bool> {
        let url = format!("{}/api/v0/block/stat?arg={}", self.config.api_url, cid);
        
        let response = self.http.post(&url)
            .send()
            .await
            .map_err(|e| GitError::IpfsError(format!("Failed to check if file exists in IPFS: {}", e)))?;
            
        Ok(response.status().is_success())
    }
    
    /// Pin a file in IPFS
    pub async fn pin(&self, cid: &str) -> Result<()> {
        let url = format!("{}/api/v0/pin/add?arg={}", self.config.api_url, cid);
        
        let response = self.http.post(&url)
            .send()
            .await
            .map_err(|e| GitError::IpfsError(format!("Failed to pin file: {}", e)))?;
            
        if !response.status().is_success() {
            let error = response.text().await
                .unwrap_or_else(|_| "Unknown error".to_string());
                
            return Err(GitError::IpfsError(format!("IPFS pin failed: {}", error)));
        }
        
        Ok(())
    }
    
    /// Unpin a file in IPFS
    pub async fn unpin(&self, cid: &str) -> Result<()> {
        let url = format!("{}/api/v0/pin/rm?arg={}", self.config.api_url, cid);
        
        let response = self.http.post(&url)
            .send()
            .await
            .map_err(|e| GitError::IpfsError(format!("Failed to unpin file: {}", e)))?;
            
        if !response.status().is_success() {
            let error = response.text().await
                .unwrap_or_else(|_| "Unknown error".to_string());
                
            return Err(GitError::IpfsError(format!("IPFS unpin failed: {}", error)));
        }
        
        Ok(())
    }
    
    /// List all pinned files in IPFS
    pub async fn list_pins(&self) -> Result<Vec<String>> {
        let url = format!("{}/api/v0/pin/ls", self.config.api_url);
        
        let response = self.http.post(&url)
            .send()
            .await
            .map_err(|e| GitError::IpfsError(format!("Failed to list pins: {}", e)))?;
            
        if !response.status().is_success() {
            let error = response.text().await
                .unwrap_or_else(|_| "Unknown error".to_string());
                
            return Err(GitError::IpfsError(format!("IPFS pin ls failed: {}", error)));
        }
        
        let json: Value = response.json().await
            .map_err(|e| GitError::IpfsError(format!("Failed to parse IPFS response: {}", e)))?;
            
        let pins = match json.get("Keys") {
            Some(keys) => {
                // Get all the CIDs from the keys
                keys.as_object()
                    .map(|obj| obj.keys().cloned().collect())
                    .unwrap_or_default()
            }
            None => Vec::new(),
        };
        
        Ok(pins)
    }
    
    /// Get config for this client
    pub fn config(&self) -> &IpfsConfig {
        &self.config
    }
    
    /// Get a mutable reference to the config
    pub fn config_mut(&mut self) -> &mut IpfsConfig {
        &mut self.config
    }
}