/// Git LFS client implementation
///
/// This module provides a client for interacting with Git LFS servers and IPFS
use std::path::{Path, PathBuf};
use std::sync::Arc;

use reqwest::{Client as HttpClient, Response};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE, ACCEPT};
use serde::{Serialize, Deserialize};
use bytes::Bytes;
use tokio::fs as tokio_fs;
use tokio::io::AsyncWriteExt;

use crate::core::{GitError, Result};
use crate::ipfs::IpfsClient;
use super::{LfsConfig, LfsPointer};

/// LFS batch request object
#[derive(Debug, Serialize)]
struct BatchRequestObject {
    oid: String,
    size: u64,
}

/// LFS batch request
#[derive(Debug, Serialize)]
struct BatchRequest<'a> {
    operation: &'a str,
    #[serde(rename = "ref")]
    reference: Option<GitReference<'a>>,
    objects: Vec<BatchRequestObject>,
    transfers: Option<Vec<&'a str>>,
}

/// Git reference information
#[derive(Debug, Serialize)]
struct GitReference<'a> {
    name: &'a str,
}

/// LFS batch response
#[derive(Debug, Deserialize)]
struct BatchResponse {
    transfer: Option<String>,
    objects: Vec<BatchResponseObject>,
}

/// LFS batch response object
#[derive(Debug, Deserialize)]
struct BatchResponseObject {
    oid: String,
    size: u64,
    authenticated: Option<bool>,
    actions: Option<std::collections::HashMap<String, BatchObjectAction>>,
    error: Option<BatchObjectError>,
}

/// LFS batch object action
#[derive(Debug, Deserialize)]
struct BatchObjectAction {
    href: String,
    header: Option<std::collections::HashMap<String, String>>,
    expires_in: Option<u64>,
}

/// LFS batch object error
#[derive(Debug, Deserialize)]
struct BatchObjectError {
    code: u32,
    message: String,
}

/// Client for interacting with Git LFS servers
pub struct LfsClient {
    /// LFS configuration
    config: LfsConfig,
    
    /// HTTP client for API calls
    http: HttpClient,
    
    /// IPFS client for IPFS-based operations (optional)
    ipfs_client: Option<Arc<IpfsClient>>,
}

impl LfsClient {
    /// Create a new LFS client
    pub fn new(config: LfsConfig) -> Result<Self> {
        // Create HTTP client
        let http = HttpClient::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| GitError::LfsError(format!("Failed to create HTTP client: {}", e)))?;
            
        // Create client
        let client = Self {
            config,
            http,
            ipfs_client: None,
        };
        
        Ok(client)
    }
    
    /// Create a new LFS client with IPFS integration
    pub fn with_ipfs(config: LfsConfig, ipfs_client: Arc<IpfsClient>) -> Result<Self> {
        // Create HTTP client
        let http = HttpClient::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| GitError::LfsError(format!("Failed to create HTTP client: {}", e)))?;
            
        // Create client
        let client = Self {
            config,
            http,
            ipfs_client: Some(ipfs_client),
        };
        
        Ok(client)
    }
    
    /// Initialize Git LFS in a repository
    pub async fn initialize(&self, repo_path: impl AsRef<Path>) -> Result<()> {
        let repo_path = repo_path.as_ref();
        
        // Check if the repository exists
        if !repo_path.exists() {
            return Err(GitError::LfsError(format!("Repository directory does not exist: {}", repo_path.display())));
        }
        
        // Install LFS filters and hooks
        super::filter::install_filter_in_repo(repo_path)?;
        
        // Create LFS directory structure
        let lfs_path = repo_path.join(".git").join("lfs");
        tokio_fs::create_dir_all(&lfs_path).await
            .map_err(|e| GitError::LfsError(format!("Failed to create LFS directory: {}", e)))?;
            
        // Create .lfsconfig file in the repository
        let lfsconfig_path = repo_path.join(".lfsconfig");
        if !lfsconfig_path.exists() {
            let mut config_content = String::new();
            
            // Add LFS URL if configured
            if let Some(url) = &self.config.url {
                config_content.push_str(&format!("[lfs]\n    url = {}\n", url));
            }
            
            // Add IPFS settings if enabled
            if self.config.use_ipfs {
                config_content.push_str("[lfs \"artigit\"]\n    useIpfs = true\n");
            }
            
            // Write the config file
            tokio_fs::write(&lfsconfig_path, config_content).await
                .map_err(|e| GitError::LfsError(format!("Failed to write .lfsconfig: {}", e)))?;
        }
        
        println!("Git LFS initialized in {}", repo_path.display());
        Ok(())
    }
    
    /// Check if a file should be tracked by LFS based on patterns
    pub fn should_track(&self, path: impl AsRef<Path>, size: Option<u64>) -> bool {
        let path = path.as_ref();
        let path_str = path.to_string_lossy();
        
        // Check file size threshold
        if let Some(size) = size {
            if size >= self.config.size_threshold {
                return true;
            }
        }
        
        // Check patterns - if no patterns are defined, don't track by default
        if self.config.track_patterns.is_empty() {
            return false;
        }
        
        // Check for a match in the track patterns
        for pattern in &self.config.track_patterns {
            if glob::Pattern::new(pattern).ok().map_or(false, |p| p.matches(&path_str)) {
                return true;
            }
        }
        
        false
    }
    
    /// Get LFS server URL
    pub fn get_server_url(&self) -> Option<&str> {
        self.config.url.as_deref()
    }
    
    /// Upload a file to LFS
    pub async fn upload_file(&self, file_path: impl AsRef<Path>) -> Result<LfsPointer> {
        let file_path = file_path.as_ref();
        
        // Read the file
        let data = tokio_fs::read(file_path).await
            .map_err(|e| GitError::LfsError(format!("Failed to read file: {}", e)))?;
            
        // Calculate SHA-256 hash
        let mut hasher = sha2::Sha256::new();
        use sha2::Digest;
        hasher.update(&data);
        let hash = format!("{:x}", hasher.finalize());
        
        // Create LFS object ID and pointer
        let oid = format!("sha256:{}", hash);
        let mut pointer = LfsPointer::new(&oid, data.len() as u64);
        
        // If IPFS is enabled, upload to IPFS
        if self.config.use_ipfs {
            if let Some(ipfs_client) = &self.ipfs_client {
                match self.upload_to_ipfs(&data).await {
                    Ok(cid) => pointer.set_ipfs_cid(&cid),
                    Err(e) => eprintln!("Warning: Failed to upload to IPFS: {}", e),
                }
            }
        }
        
        // If we have an LFS server URL, also upload to LFS server
        if let Some(url) = &self.config.url {
            self.upload_to_server(&pointer, &data).await?;
        }
        
        Ok(pointer)
    }
    
    /// Upload a file to an LFS server
    pub async fn upload_to_server(&self, pointer: &LfsPointer, data: &[u8]) -> Result<()> {
        let server_url = self.config.url.as_ref()
            .ok_or_else(|| GitError::LfsError("LFS server URL not configured".to_string()))?;
            
        // First, make a batch request to check if the server already has the object
        let batch_url = format!("{}/objects/batch", server_url);
        
        let request = BatchRequest {
            operation: "upload",
            reference: None,
            objects: vec![BatchRequestObject {
                oid: pointer.oid.clone(),
                size: pointer.size,
            }],
            transfers: Some(vec!["basic"]),
        };
        
        let response = self.http.post(&batch_url)
            .header(CONTENT_TYPE, "application/vnd.git-lfs+json")
            .header(ACCEPT, "application/vnd.git-lfs+json")
            .json(&request)
            .send()
            .await
            .map_err(|e| GitError::LfsError(format!("LFS batch request failed: {}", e)))?;
            
        if !response.status().is_success() {
            let error = response.text().await
                .unwrap_or_else(|_| "Unknown error".to_string());
                
            return Err(GitError::LfsError(format!("LFS server error: {}", error)));
        }
        
        // Parse the batch response
        let batch_response: BatchResponse = response.json().await
            .map_err(|e| GitError::LfsError(format!("Failed to parse LFS response: {}", e)))?;
            
        // Find our object in the response
        if let Some(obj) = batch_response.objects.iter().find(|o| o.oid == pointer.oid) {
            // Check for errors
            if let Some(error) = &obj.error {
                return Err(GitError::LfsError(format!("LFS error: {} - {}", error.code, error.message)));
            }
            
            // Check if we need to upload the object
            if let Some(actions) = &obj.actions {
                if let Some(upload_action) = actions.get("upload") {
                    // We need to upload the object
                    let upload_url = &upload_action.href;
                    
                    // Create headers from the action if any
                    let mut headers = HeaderMap::new();
                    if let Some(action_headers) = &upload_action.header {
                        for (key, value) in action_headers {
                            headers.insert(
                                reqwest::header::HeaderName::from_bytes(key.as_bytes())
                                    .map_err(|_| GitError::LfsError(format!("Invalid header name: {}", key)))?,
                                HeaderValue::from_str(value)
                                    .map_err(|_| GitError::LfsError(format!("Invalid header value: {}", value)))?,
                            );
                        }
                    }
                    
                    // Make the upload request
                    let response = self.http.put(upload_url)
                        .headers(headers)
                        .body(data.to_vec())
                        .send()
                        .await
                        .map_err(|e| GitError::LfsError(format!("LFS upload failed: {}", e)))?;
                        
                    if !response.status().is_success() {
                        let error = response.text().await
                            .unwrap_or_else(|_| "Unknown error".to_string());
                            
                        return Err(GitError::LfsError(format!("LFS upload error: {}", error)));
                    }
                    
                    println!("Successfully uploaded {} to LFS server", pointer.oid);
                }
            }
        } else {
            return Err(GitError::LfsError("Object not found in LFS batch response".to_string()));
        }
        
        Ok(())
    }
    
    /// Upload data to IPFS and return the CID
    pub async fn upload_to_ipfs(&self, data: &[u8]) -> Result<String> {
        let ipfs_client = self.ipfs_client.as_ref()
            .ok_or_else(|| GitError::LfsError("IPFS client not configured".to_string()))?;
            
        ipfs_client.add_bytes(data).await
    }
    
    /// Get an object from either IPFS or LFS server
    pub async fn get_object(&self, pointer: &LfsPointer, dest_path: impl AsRef<Path>) -> Result<()> {
        let dest_path = dest_path.as_ref();
        
        // First try IPFS if we have a CID
        if let Some(cid) = &pointer.ipfs_cid {
            if let Some(ipfs_client) = &self.ipfs_client {
                match ipfs_client.get_file(cid).await {
                    Ok(data) => {
                        tokio_fs::write(dest_path, data).await
                            .map_err(|e| GitError::LfsError(format!("Failed to write file: {}", e)))?;
                            
                        return Ok(());
                    },
                    Err(e) => {
                        eprintln!("Warning: Failed to get object from IPFS: {}", e);
                        // Continue to try LFS server
                    }
                }
            }
        }
        
        // If IPFS failed or wasn't available, try the LFS server
        if let Some(url) = &self.config.url {
            self.get_from_server(pointer, dest_path).await?;
        } else {
            return Err(GitError::LfsError(format!("Could not find LFS object {}", pointer.oid)));
        }
        
        Ok(())
    }
    
    /// Download an object from an LFS server
    pub async fn get_from_server(&self, pointer: &LfsPointer, dest_path: impl AsRef<Path>) -> Result<()> {
        let server_url = self.config.url.as_ref()
            .ok_or_else(|| GitError::LfsError("LFS server URL not configured".to_string()))?;
            
        // First, make a batch request to get the download URL
        let batch_url = format!("{}/objects/batch", server_url);
        
        let request = BatchRequest {
            operation: "download",
            reference: None,
            objects: vec![BatchRequestObject {
                oid: pointer.oid.clone(),
                size: pointer.size,
            }],
            transfers: Some(vec!["basic"]),
        };
        
        let response = self.http.post(&batch_url)
            .header(CONTENT_TYPE, "application/vnd.git-lfs+json")
            .header(ACCEPT, "application/vnd.git-lfs+json")
            .json(&request)
            .send()
            .await
            .map_err(|e| GitError::LfsError(format!("LFS batch request failed: {}", e)))?;
            
        if !response.status().is_success() {
            let error = response.text().await
                .unwrap_or_else(|_| "Unknown error".to_string());
                
            return Err(GitError::LfsError(format!("LFS server error: {}", error)));
        }
        
        // Parse the batch response
        let batch_response: BatchResponse = response.json().await
            .map_err(|e| GitError::LfsError(format!("Failed to parse LFS response: {}", e)))?;
            
        // Find our object in the response
        if let Some(obj) = batch_response.objects.iter().find(|o| o.oid == pointer.oid) {
            // Check for errors
            if let Some(error) = &obj.error {
                return Err(GitError::LfsError(format!("LFS error: {} - {}", error.code, error.message)));
            }
            
            // Check if we need to download the object
            if let Some(actions) = &obj.actions {
                if let Some(download_action) = actions.get("download") {
                    // We need to download the object
                    let download_url = &download_action.href;
                    
                    // Create headers from the action if any
                    let mut headers = HeaderMap::new();
                    if let Some(action_headers) = &download_action.header {
                        for (key, value) in action_headers {
                            headers.insert(
                                reqwest::header::HeaderName::from_bytes(key.as_bytes())
                                    .map_err(|_| GitError::LfsError(format!("Invalid header name: {}", key)))?,
                                HeaderValue::from_str(value)
                                    .map_err(|_| GitError::LfsError(format!("Invalid header value: {}", value)))?,
                            );
                        }
                    }
                    
                    // Make the download request
                    let response = self.http.get(download_url)
                        .headers(headers)
                        .send()
                        .await
                        .map_err(|e| GitError::LfsError(format!("LFS download failed: {}", e)))?;
                        
                    if !response.status().is_success() {
                        let error = response.text().await
                            .unwrap_or_else(|_| "Unknown error".to_string());
                            
                        return Err(GitError::LfsError(format!("LFS download error: {}", error)));
                    }
                    
                    // Get the content and write to file
                    let content = response.bytes().await
                        .map_err(|e| GitError::LfsError(format!("Failed to read LFS response: {}", e)))?;
                        
                    tokio_fs::write(dest_path, content).await
                        .map_err(|e| GitError::LfsError(format!("Failed to write file: {}", e)))?;
                        
                    // If IPFS is enabled, also store in IPFS for future use
                    if self.config.use_ipfs {
                        if let Some(ipfs_client) = &self.ipfs_client {
                            match ipfs_client.add_file(dest_path).await {
                                Ok(cid) => {
                                    println!("Stored LFS object {} in IPFS with CID: {}", pointer.oid, cid);
                                    // In a real-world implementation, we would save this CID mapping
                                },
                                Err(e) => eprintln!("Warning: Failed to upload to IPFS: {}", e),
                            }
                        }
                    }
                    
                    println!("Successfully downloaded {} from LFS server", pointer.oid);
                    return Ok(());
                }
            }
        }
        
        Err(GitError::LfsError(format!("Could not find LFS object {}", pointer.oid)))
    }
    
    /// Track a pattern with Git LFS
    pub async fn track(&self, pattern: &str, repo_path: impl AsRef<Path>) -> Result<()> {
        let repo_path = repo_path.as_ref();
        
        // First make sure LFS is initialized
        self.initialize(repo_path).await?;
        
        // Update .gitattributes file
        let gitattributes_path = repo_path.join(".gitattributes");
        
        let pattern_line = format!("{} filter=lfs diff=lfs merge=lfs -text\n", pattern);
        
        // Read existing content or create new file
        let content = if gitattributes_path.exists() {
            let mut existing = tokio_fs::read_to_string(&gitattributes_path).await
                .map_err(|e| GitError::LfsError(format!("Failed to read .gitattributes: {}", e)))?;
                
            // Check if pattern is already tracked
            if existing.contains(&pattern_line) {
                println!("Pattern '{}' is already tracked by Git LFS", pattern);
                return Ok(());
            }
            
            // Add new pattern
            if !existing.ends_with('\n') {
                existing.push('\n');
            }
            existing.push_str(&pattern_line);
            existing
        } else {
            pattern_line
        };
        
        // Write to file
        tokio_fs::write(&gitattributes_path, content).await
            .map_err(|e| GitError::LfsError(format!("Failed to write .gitattributes: {}", e)))?;
            
        println!("Now tracking '{}' with Git LFS", pattern);
        Ok(())
    }
    
    /// Get the current configuration
    pub fn config(&self) -> &LfsConfig {
        &self.config
    }
    
    /// Get a mutable reference to the configuration
    pub fn config_mut(&mut self) -> &mut LfsConfig {
        &mut self.config
    }
}