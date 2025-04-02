/// IPFS client for ArtiGit
///
/// This module provides a client for interacting with IPFS
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::io::{self, Read};
use bytes::Bytes;
use reqwest::Client as HttpClient;
use reqwest::multipart;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, AsyncReadExt, AsyncWriteExt};
use tokio::fs::File;
use tokio_util::codec::{BytesCodec, FramedRead};
use futures::StreamExt;

use crate::core::{GitError, Result};
use super::IpfsConfig;

/// Standard chunk size for large files (1MB)
const DEFAULT_CHUNK_SIZE: usize = 1024 * 1024;

/// Client for interacting with IPFS nodes
#[derive(Debug, Clone)]
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

/// IPFS directory listing response
#[derive(Debug, Deserialize)]
struct IpfsLsResponse {
    /// Objects in the directory
    #[serde(rename = "Objects")]
    objects: Vec<IpfsObject>,
}

/// IPFS object with links
#[derive(Debug, Deserialize)]
struct IpfsObject {
    /// Hash of the object
    #[serde(rename = "Hash")]
    hash: String,
    
    /// Links to other objects
    #[serde(rename = "Links")]
    links: Vec<IpfsLink>,
}

/// IPFS link to another object
#[derive(Debug, Deserialize)]
struct IpfsLink {
    /// Name of the link
    #[serde(rename = "Name")]
    name: String,
    
    /// Hash of the linked object
    #[serde(rename = "Hash")]
    hash: String,
    
    /// Size of the linked object
    #[serde(rename = "Size")]
    size: usize,
}

/// IPFS DAG object
#[derive(Debug, Serialize, Deserialize)]
pub struct IpfsDagNode {
    /// Links to other objects
    #[serde(rename = "Links")]
    pub links: Vec<IpfsDagLink>,
    
    /// Data contained in this node
    #[serde(rename = "Data")]
    pub data: Option<String>,
}

/// Link in an IPFS DAG node
#[derive(Debug, Serialize, Deserialize)]
pub struct IpfsDagLink {
    /// Name of the link
    #[serde(rename = "Name")]
    pub name: String,
    
    /// Hash of the linked object
    #[serde(rename = "Cid")]
    pub cid: String,
    
    /// Size of the linked object
    #[serde(rename = "Size")]
    pub size: usize,
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
        
        // If the file is larger than the chunking threshold, use chunked upload
        let metadata = tokio::fs::metadata(path).await
            .map_err(|e| GitError::IpfsError(format!("Failed to get file metadata: {}", e)))?;
            
        if metadata.len() > self.config.chunking_threshold as u64 {
            log::info!("File {} is large ({}MB), using chunked upload", 
                     path.display(), metadata.len() / 1024 / 1024);
            return self.add_large_file(path).await;
        }
        
        // Regular upload for standard files
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
        let url = format!("{}/api/v0/add?pin={}", 
                         self.config.api_url, 
                         if self.config.auto_pin { "true" } else { "false" });
        
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
            
        Ok(add_response.hash)
    }
    
    /// Add a large file to IPFS using chunking for better performance
    async fn add_large_file(&self, path: impl AsRef<Path>) -> Result<String> {
        let path = path.as_ref();
        let file_name = path.file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        
        // Open the file
        let file = File::open(path).await
            .map_err(|e| GitError::IpfsError(format!("Failed to open file: {}", e)))?;
        
        // Get file size
        let file_size = file.metadata().await
            .map_err(|e| GitError::IpfsError(format!("Failed to get file metadata: {}", e)))?
            .len();
        
        // Calculate optimal chunk size based on file size
        let chunk_size = if file_size > 100 * 1024 * 1024 {
            // For files over 100MB, use 5MB chunks
            5 * 1024 * 1024
        } else {
            // Otherwise use the default chunk size
            DEFAULT_CHUNK_SIZE
        };
        
        log::info!("Uploading {} ({}MB) in {}MB chunks", 
                 file_name, file_size / 1024 / 1024, chunk_size / 1024 / 1024);
        
        // Create chunked reader
        let file_stream = FramedRead::new(file, BytesCodec::new());
        
        // Build the form with the file stream
        let form = multipart::Form::new()
            .part("file", multipart::Part::stream(file_stream).file_name(file_name));
        
        // Make the API request with chunked=true
        let url = format!("{}/api/v0/add?chunker=size-{}&pin={}", 
                         self.config.api_url, 
                         chunk_size,
                         if self.config.auto_pin { "true" } else { "false" });
        
        let response = self.http.post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| GitError::IpfsError(format!("Failed to upload chunked file to IPFS: {}", e)))?;
            
        if !response.status().is_success() {
            let error = response.text().await
                .unwrap_or_else(|_| "Unknown error".to_string());
                
            return Err(GitError::IpfsError(format!("IPFS chunked add failed: {}", error)));
        }
        
        // Parse the response (multiple JSON objects for chunks, last one is the final result)
        let text = response.text().await
            .map_err(|e| GitError::IpfsError(format!("Failed to read IPFS response: {}", e)))?;
        
        // The response contains multiple JSON objects, one per line
        // The last one contains the final hash
        let lines: Vec<&str> = text.lines().collect();
        if lines.is_empty() {
            return Err(GitError::IpfsError("Empty response from IPFS".to_string()));
        }
        
        // Parse the last line to get the final hash
        let last_line = lines.last().unwrap();
        let add_response: AddResponse = serde_json::from_str(last_line)
            .map_err(|e| GitError::IpfsError(format!("Failed to parse IPFS response: {}", e)))?;
        
        log::info!("Successfully uploaded {} to IPFS with CID: {}", file_name, add_response.hash);
        
        Ok(add_response.hash)
    }
    
    /// Add raw bytes to IPFS
    pub async fn add_bytes(&self, data: &[u8]) -> Result<String> {
        // Build the form with the data
        let form = multipart::Form::new()
            .part("file", multipart::Part::bytes(data.to_vec()).file_name("data"));
            
        // Make the API request
        let url = format!("{}/api/v0/add?pin={}", 
                         self.config.api_url, 
                         if self.config.auto_pin { "true" } else { "false" });
        
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
            
        Ok(add_response.hash)
    }
    
    /// Add a directory to IPFS
    pub async fn add_directory(&self, dir_path: impl AsRef<Path>) -> Result<String> {
        let dir_path = dir_path.as_ref();
        
        // Ensure the directory exists
        if !dir_path.exists() || !dir_path.is_dir() {
            return Err(GitError::IpfsError(
                format!("Directory does not exist: {}", dir_path.display())
            ));
        }
        
        // Create a recursive form with the directory
        // Note: IPFS API doesn't directly support directory uploads via API
        // We need to use the ipfs daemon's "add -r" functionality
        
        // For now, we'll implement a simplified version that adds files individually
        // and then creates a directory structure using IPFS DAG API
        
        // Walk the directory recursively and add each file
        let mut directory_structure = HashMap::new();
        self.add_directory_recursive(dir_path, dir_path, &mut directory_structure).await?;
        
        // Create the root DAG node
        let root_name = dir_path.file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "root".to_string());
            
        // Build the DAG links from our directory structure
        let mut dag_links = Vec::new();
        
        for (path, cid) in directory_structure {
            let relative_path = path.strip_prefix(dir_path)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
                
            // Add link to the DAG node
            dag_links.push(IpfsDagLink {
                name: relative_path,
                cid,
                size: 0,  // We don't have the size information here
            });
        }
        
        // Create the root DAG node
        let root_node = IpfsDagNode {
            links: dag_links,
            data: Some("directory".to_string()),
        };
        
        // Serialize the DAG node
        let node_json = serde_json::to_string(&root_node)
            .map_err(|e| GitError::IpfsError(format!("Failed to serialize DAG node: {}", e)))?;
            
        // Add the DAG node to IPFS
        let url = format!("{}/api/v0/dag/put?pin={}", 
                         self.config.api_url,
                         if self.config.auto_pin { "true" } else { "false" });
                         
        let form = multipart::Form::new()
            .part("file", multipart::Part::bytes(node_json.into_bytes()).file_name("dag.json"));
            
        let response = self.http.post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| GitError::IpfsError(format!("Failed to create DAG node: {}", e)))?;
            
        if !response.status().is_success() {
            let error = response.text().await
                .unwrap_or_else(|_| "Unknown error".to_string());
                
            return Err(GitError::IpfsError(format!("IPFS DAG put failed: {}", error)));
        }
        
        // Parse the response to get the CID
        let json: Value = response.json().await
            .map_err(|e| GitError::IpfsError(format!("Failed to parse DAG put response: {}", e)))?;
            
        let cid = json["Cid"]["/"].as_str()
            .ok_or_else(|| GitError::IpfsError("Invalid DAG put response".to_string()))?;
            
        Ok(cid.to_string())
    }
    
    /// Recursively add a directory structure to IPFS
    async fn add_directory_recursive(
        &self,
        base_path: &Path,
        current_path: &Path,
        structure: &mut HashMap<PathBuf, String>
    ) -> Result<()> {
        // List all entries in the current directory
        let mut entries = tokio::fs::read_dir(current_path).await
            .map_err(|e| GitError::IpfsError(format!("Failed to read directory: {}", e)))?;
            
        while let Some(entry) = entries.next_entry().await
            .map_err(|e| GitError::IpfsError(format!("Failed to read directory entry: {}", e)))? {
            
            let path = entry.path();
            
            if path.is_file() {
                // Add the file to IPFS
                let cid = self.add_file(&path).await?;
                
                // Store the CID in the structure
                structure.insert(path, cid);
            } else if path.is_dir() {
                // Recursively process subdirectories
                self.add_directory_recursive(base_path, &path, structure).await?;
            }
        }
        
        Ok(())
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
    
    /// Get a file from IPFS and save it to a local path
    pub async fn get_file_to_path(&self, cid: &str, output_path: impl AsRef<Path>) -> Result<()> {
        let url = format!("{}/api/v0/cat?arg={}", self.config.api_url, cid);
        let output_path = output_path.as_ref();
        
        // Create parent directories if needed
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent).await
                .map_err(|e| GitError::IpfsError(format!("Failed to create directories: {}", e)))?;
        }
        
        // Stream the file to disk to handle large files efficiently
        let response = self.http.post(&url)
            .send()
            .await
            .map_err(|e| GitError::IpfsError(format!("Failed to get file from IPFS: {}", e)))?;
            
        if !response.status().is_success() {
            let error = response.text().await
                .unwrap_or_else(|_| "Unknown error".to_string());
                
            return Err(GitError::IpfsError(format!("IPFS cat failed: {}", error)));
        }
        
        // Create and open the output file
        let mut file = File::create(output_path).await
            .map_err(|e| GitError::IpfsError(format!("Failed to create output file: {}", e)))?;
            
        // Stream the response body to the file
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| GitError::IpfsError(format!("Failed to read chunk: {}", e)))?;
            file.write_all(&chunk).await
                .map_err(|e| GitError::IpfsError(format!("Failed to write to file: {}", e)))?;
        }
        
        // Ensure all data is written
        file.flush().await
            .map_err(|e| GitError::IpfsError(format!("Failed to flush file: {}", e)))?;
            
        Ok(())
    }
    
    /// Get a directory from IPFS and save it to a local path
    pub async fn get_directory(&self, cid: &str, output_dir: impl AsRef<Path>) -> Result<()> {
        let output_dir = output_dir.as_ref();
        
        // Create the output directory if it doesn't exist
        tokio::fs::create_dir_all(output_dir).await
            .map_err(|e| GitError::IpfsError(format!("Failed to create output directory: {}", e)))?;
            
        // First, check if this is a DAG node
        let dag_url = format!("{}/api/v0/dag/get?arg={}", self.config.api_url, cid);
        
        let dag_response = self.http.post(&dag_url)
            .send()
            .await
            .map_err(|e| GitError::IpfsError(format!("Failed to get DAG node: {}", e)))?;
            
        if dag_response.status().is_success() {
            // This is a DAG node, parse it and process its links
            let dag_node: IpfsDagNode = dag_response.json().await
                .map_err(|e| GitError::IpfsError(format!("Failed to parse DAG node: {}", e)))?;
                
            // Process each link in the DAG node
            for link in dag_node.links {
                let file_path = output_dir.join(&link.name);
                
                // Ensure parent directories exist
                if let Some(parent) = file_path.parent() {
                    tokio::fs::create_dir_all(parent).await
                        .map_err(|e| GitError::IpfsError(format!("Failed to create directories: {}", e)))?;
                }
                
                // Get the linked file
                self.get_file_to_path(&link.cid, &file_path).await?;
            }
            
            return Ok(());
        }
        
        // If it's not a DAG node, try listing it as a directory
        let ls_url = format!("{}/api/v0/ls?arg={}", self.config.api_url, cid);
        
        let ls_response = self.http.post(&ls_url)
            .send()
            .await
            .map_err(|e| GitError::IpfsError(format!("Failed to list directory: {}", e)))?;
            
        if !ls_response.status().is_success() {
            let error = ls_response.text().await
                .unwrap_or_else(|_| "Unknown error".to_string());
                
            return Err(GitError::IpfsError(format!("IPFS ls failed: {}", error)));
        }
        
        // Parse the directory listing
        let ls_result: IpfsLsResponse = ls_response.json().await
            .map_err(|e| GitError::IpfsError(format!("Failed to parse directory listing: {}", e)))?;
            
        // Process each object in the directory
        for object in ls_result.objects {
            for link in object.links {
                let file_path = output_dir.join(&link.name);
                
                // Get the linked file/directory
                if link.name.is_empty() {
                    // This is a file, not a directory
                    self.get_file_to_path(&link.hash, &file_path).await?;
                } else {
                    // Create subdirectory and recursively get its contents
                    tokio::fs::create_dir_all(&file_path).await
                        .map_err(|e| GitError::IpfsError(format!("Failed to create directory: {}", e)))?;
                        
                    self.get_directory(&link.hash, &file_path).await?;
                }
            }
        }
        
        Ok(())
    }
    
    /// List the contents of a directory in IPFS
    pub async fn list_directory(&self, cid: &str) -> Result<Vec<(String, String)>> {
        // First try listing as a standard IPFS directory
        let url = format!("{}/api/v0/ls?arg={}", self.config.api_url, cid);
        
        let response = self.http.post(&url)
            .send()
            .await
            .map_err(|e| GitError::IpfsError(format!("Failed to list directory: {}", e)))?;
            
        if response.status().is_success() {
            // Parse the directory listing
            let ls_result: IpfsLsResponse = response.json().await
                .map_err(|e| GitError::IpfsError(format!("Failed to parse directory listing: {}", e)))?;
                
            // Extract file names and hashes
            let mut files = Vec::new();
            for object in ls_result.objects {
                for link in object.links {
                    files.push((link.name, link.hash));
                }
            }
            
            return Ok(files);
        }
        
        // If that fails, try getting as a DAG node
        let dag_url = format!("{}/api/v0/dag/get?arg={}", self.config.api_url, cid);
        
        let dag_response = self.http.post(&dag_url)
            .send()
            .await
            .map_err(|e| GitError::IpfsError(format!("Failed to get DAG node: {}", e)))?;
            
        if dag_response.status().is_success() {
            // Parse the DAG node
            let dag_node: IpfsDagNode = dag_response.json().await
                .map_err(|e| GitError::IpfsError(format!("Failed to parse DAG node: {}", e)))?;
                
            // Extract links
            let files = dag_node.links.into_iter()
                .map(|link| (link.name, link.cid))
                .collect();
                
            return Ok(files);
        }
        
        // If both methods fail, return an error
        Err(GitError::IpfsError(format!("Failed to list directory: {}", cid)))
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
    
    /// Pin a file in IPFS recursively
    pub async fn pin_recursive(&self, cid: &str) -> Result<()> {
        let url = format!("{}/api/v0/pin/add?arg={}&recursive=true", self.config.api_url, cid);
        
        let response = self.http.post(&url)
            .send()
            .await
            .map_err(|e| GitError::IpfsError(format!("Failed to pin file recursively: {}", e)))?;
            
        if !response.status().is_success() {
            let error = response.text().await
                .unwrap_or_else(|_| "Unknown error".to_string());
                
            return Err(GitError::IpfsError(format!("IPFS recursive pin failed: {}", error)));
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
    
    /// Create a direct link to an IPFS gateway URL for a given CID
    pub fn gateway_url(&self, cid: &str) -> String {
        if self.config.gateway_url.is_empty() {
            // Use public IPFS gateway if none is configured
            format!("https://ipfs.io/ipfs/{}", cid)
        } else {
            format!("{}/ipfs/{}", self.config.gateway_url.trim_end_matches('/'), cid)
        }
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