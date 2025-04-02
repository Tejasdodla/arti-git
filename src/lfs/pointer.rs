/// Git LFS pointer file implementation
///
/// This module provides functionality for working with Git LFS pointer files.
/// A pointer file is a small text file that replaces a large file in a Git repository,
/// containing metadata about the original file and where to find it.
use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use sha1::{Sha1, Digest};

use crate::core::{GitError, Result};

/// A Git LFS pointer file
#[derive(Debug, Clone)]
pub struct LfsPointer {
    /// Version string for the LFS spec
    pub version: String,
    
    /// Object ID for the LFS object (SHA-256 hash with prefix)
    pub oid: String,
    
    /// Size of the file in bytes
    pub size: u64,
    
    /// IPFS CID for the object (if stored in IPFS)
    pub ipfs_cid: Option<String>,
    
    /// File path (optional) - used for creating extended attributes
    pub file_path: Option<String>,
    
    /// Original filename (optional)
    pub filename: Option<String>,
    
    /// MIME type (optional)
    pub mimetype: Option<String>,
    
    /// Additional custom attributes
    pub attributes: HashMap<String, String>,
}

impl LfsPointer {
    /// Create a new LFS pointer
    pub fn new(oid: &str, size: u64) -> Self {
        Self {
            version: "https://git-lfs.github.com/spec/v1".to_string(),
            oid: oid.to_string(),
            size,
            ipfs_cid: None,
            file_path: None,
            filename: None,
            mimetype: None,
            attributes: HashMap::new(),
        }
    }
    
    /// Create a new LFS pointer from file data
    pub fn from_data(data: &[u8], path: Option<impl AsRef<Path>>) -> Self {
        // Calculate SHA-256 hash of the data
        let digest = sha2::Sha256::digest(data);
        let oid = format!("sha256:{}", hex::encode(digest));
        
        let mut pointer = Self::new(&oid, data.len() as u64);
        
        // Set file path and name if provided
        if let Some(path) = path {
            let path_ref = path.as_ref();
            pointer.file_path = Some(path_ref.to_string_lossy().to_string());
            
            if let Some(filename) = path_ref.file_name() {
                pointer.filename = Some(filename.to_string_lossy().to_string());
            }
            
            // Try to determine MIME type based on extension
            if let Some(ext) = path_ref.extension() {
                let ext_str = ext.to_string_lossy().to_lowercase();
                
                // Simple mapping for common file types
                let mime = match ext_str.as_str() {
                    "png" => "image/png",
                    "jpg" | "jpeg" => "image/jpeg",
                    "gif" => "image/gif",
                    "pdf" => "application/pdf",
                    "zip" => "application/zip",
                    "txt" => "text/plain",
                    "html" | "htm" => "text/html",
                    "json" => "application/json",
                    "js" => "application/javascript",
                    "css" => "text/css",
                    "xml" => "application/xml",
                    "mp3" => "audio/mpeg",
                    "mp4" => "video/mp4",
                    "mov" => "video/quicktime",
                    _ => "application/octet-stream",
                };
                
                pointer.mimetype = Some(mime.to_string());
            }
        }
        
        pointer
    }
    
    /// Set the IPFS CID for this object
    pub fn set_ipfs_cid(&mut self, cid: &str) {
        self.ipfs_cid = Some(cid.to_string());
        
        // Also store in attributes for compatibility with standard LFS clients
        self.attributes.insert("x-artigit-ipfs-cid".to_string(), cid.to_string());
    }
    
    /// Set arbitrary metadata attribute
    pub fn set_attribute(&mut self, key: &str, value: &str) {
        self.attributes.insert(key.to_string(), value.to_string());
    }
    
    /// Parse a pointer from a string
    pub fn parse(s: &str) -> Result<Self> {
        let mut version = None;
        let mut oid = None;
        let mut size = None;
        let mut ipfs_cid = None;
        let mut file_path = None;
        let mut filename = None;
        let mut mimetype = None;
        let mut attributes = HashMap::new();
        
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            
            // Parse key-value pairs
            let parts: Vec<&str> = line.splitn(2, ' ').collect();
            if parts.len() != 2 {
                continue;
            }
            
            let key = parts[0];
            let value = parts[1];
            
            match key {
                "version" => version = Some(value.to_string()),
                "oid" => oid = Some(value.to_string()),
                "size" => {
                    size = value.parse::<u64>().ok();
                }
                "x-artigit-ipfs-cid" => {
                    ipfs_cid = Some(value.to_string());
                }
                "x-artigit-file-path" => {
                    file_path = Some(value.to_string());
                }
                "x-artigit-filename" => {
                    filename = Some(value.to_string());
                }
                "x-artigit-mimetype" => {
                    mimetype = Some(value.to_string());
                }
                _ => {
                    attributes.insert(key.to_string(), value.to_string());
                }
            }
        }
        
        // Check required fields
        let version = version.ok_or_else(|| GitError::LfsError("Missing version in LFS pointer".to_string()))?;
        let oid = oid.ok_or_else(|| GitError::LfsError("Missing oid in LFS pointer".to_string()))?;
        let size = size.ok_or_else(|| GitError::LfsError("Missing or invalid size in LFS pointer".to_string()))?;
        
        Ok(Self {
            version,
            oid,
            size,
            ipfs_cid,
            file_path,
            filename,
            mimetype,
            attributes,
        })
    }
    
    /// Convert the pointer to a string
    pub fn to_string(&self) -> String {
        let mut lines = Vec::new();
        
        lines.push(format!("version {}", self.version));
        lines.push(format!("oid {}", self.oid));
        lines.push(format!("size {}", self.size));
        
        // Add IPFS CID if available
        if let Some(cid) = &self.ipfs_cid {
            lines.push(format!("x-artigit-ipfs-cid {}", cid));
        }
        
        // Add file path if available
        if let Some(path) = &self.file_path {
            lines.push(format!("x-artigit-file-path {}", path));
        }
        
        // Add filename if available
        if let Some(name) = &self.filename {
            lines.push(format!("x-artigit-filename {}", name));
        }
        
        // Add mimetype if available
        if let Some(mime) = &self.mimetype {
            lines.push(format!("x-artigit-mimetype {}", mime));
        }
        
        // Add custom attributes
        for (key, value) in &self.attributes {
            // Skip our custom attributes as we've already added them
            if !key.starts_with("x-artigit-") {
                lines.push(format!("{} {}", key, value));
            }
        }
        
        lines.join("\n")
    }
    
    /// Get the URL for this pointer
    pub fn url(&self, base_url: &str) -> String {
        // Handle both local and remote URLs
        if base_url.starts_with("http") {
            // For remote HTTP URLs
            format!("{}/objects/{}", base_url.trim_end_matches('/'), self.object_path())
        } else {
            // For local file paths
            let base = Path::new(base_url);
            let object_path = Path::new(&self.object_path());
            base.join(object_path).to_string_lossy().to_string()
        }
    }
    
    /// Get the relative path to the object within the LFS store
    pub fn object_path(&self) -> String {
        // Format: OID prefix / OID suffix
        // This follows the same structure used by Git LFS
        
        // The OID is in the format "sha256:abcdef123..."
        let hash = self.oid.split(':').nth(1).unwrap_or(&self.oid);
        
        if hash.len() < 5 {
            // For very short hashes, don't use a prefix/suffix structure
            return hash.to_string();
        }
        
        let prefix = &hash[0..2];
        let suffix = &hash[2..];
        
        format!("{}/{}", prefix, suffix)
    }
    
    /// Check if this pointer can use IPFS for retrieval
    pub fn has_ipfs(&self) -> bool {
        self.ipfs_cid.is_some()
    }
    
    /// Extract the hash algorithm and digest from the OID
    pub fn extract_digest(&self) -> Result<(String, String)> {
        let parts: Vec<&str> = self.oid.splitn(2, ':').collect();
        if parts.len() != 2 {
            return Err(GitError::LfsError(format!("Invalid OID format: {}", self.oid)));
        }
        
        Ok((parts[0].to_string(), parts[1].to_string()))
    }
    
    /// Verify data against this pointer
    pub fn verify(&self, data: &[u8]) -> Result<bool> {
        // Verify the size first (quick check)
        if data.len() as u64 != self.size {
            return Ok(false);
        }
        
        // Extract the hash algorithm and digest
        let (algo, expected_digest) = self.extract_digest()?;
        
        // Calculate the hash based on the algorithm
        let actual_digest = match algo.as_str() {
            "sha256" => {
                let digest = sha2::Sha256::digest(data);
                hex::encode(digest)
            },
            "sha1" => {
                let digest = Sha1::digest(data);
                hex::encode(digest)
            },
            _ => return Err(GitError::LfsError(format!("Unsupported hash algorithm: {}", algo)))
        };
        
        Ok(expected_digest == actual_digest)
    }
    
    /// Create a pointer from a file
    pub async fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path_ref = path.as_ref();
        log::debug!("Reading LFS pointer from file: {}", path_ref.display());
        
        let content = tokio::fs::read_to_string(path_ref).await
            .map_err(|e| GitError::LfsError(format!("Failed to read LFS pointer: {}", e)))?;
            
        let mut pointer = Self::parse(&content)?;
        
        // Store the original path if not already present
        if pointer.file_path.is_none() {
            pointer.file_path = Some(path_ref.to_string_lossy().to_string());
        }
        
        // Store the original filename if not already present
        if pointer.filename.is_none() {
            if let Some(filename) = path_ref.file_name() {
                pointer.filename = Some(filename.to_string_lossy().to_string());
            }
        }
        
        Ok(pointer)
    }
    
    /// Write a pointer to a file
    pub async fn write_to_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let path_ref = path.as_ref();
        log::debug!("Writing LFS pointer to file: {}", path_ref.display());
        
        // Ensure parent directory exists
        if let Some(parent) = path_ref.parent() {
            if !parent.exists() {
                tokio::fs::create_dir_all(parent).await
                    .map_err(|e| GitError::LfsError(format!("Failed to create directory: {}", e)))?;
            }
        }
        
        tokio::fs::write(path_ref, self.to_string()).await
            .map_err(|e| GitError::LfsError(format!("Failed to write LFS pointer: {}", e)))?;
            
        Ok(())
    }
}

impl std::fmt::Display for LfsPointer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_string())
    }
}

impl FromStr for LfsPointer {
    type Err = GitError;
    
    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_pointer_parse() {
        let content = "version https://git-lfs.github.com/spec/v1\noid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\nsize 12345\nx-artigit-ipfs-cid QmZ9TxrEf9ZmADvjhye4tbYMQR7VuAGyvAMZNWMXXrKGFy";
        
        let pointer = LfsPointer::parse(content).unwrap();
        assert_eq!(pointer.version, "https://git-lfs.github.com/spec/v1");
        assert_eq!(pointer.oid, "sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393");
        assert_eq!(pointer.size, 12345);
        assert_eq!(pointer.ipfs_cid, Some("QmZ9TxrEf9ZmADvjhye4tbYMQR7VuAGyvAMZNWMXXrKGFy".to_string()));
    }
    
    #[test]
    fn test_pointer_to_string() {
        let mut pointer = LfsPointer::new("sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393", 12345);
        pointer.set_ipfs_cid("QmZ9TxrEf9ZmADvjhye4tbYMQR7VuAGyvAMZNWMXXrKGFy");
        
        let content = pointer.to_string();
        assert!(content.contains("version https://git-lfs.github.com/spec/v1"));
        assert!(content.contains("oid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393"));
        assert!(content.contains("size 12345"));
        assert!(content.contains("x-artigit-ipfs-cid QmZ9TxrEf9ZmADvjhye4tbYMQR7VuAGyvAMZNWMXXrKGFy"));
    }
    
    #[test]
    fn test_object_path() {
        let pointer = LfsPointer::new("sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393", 12345);
        assert_eq!(pointer.object_path(), "4d/7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393");
    }
    
    #[test]
    fn test_verify() {
        let data = b"Hello, world!";
        let pointer = LfsPointer::from_data(data, None::<&Path>);
        
        assert!(pointer.verify(data).unwrap());
        assert!(!pointer.verify(b"Modified data!").unwrap());
    }
}