/// Git LFS pointer file implementation
///
/// This module provides functionality for working with Git LFS pointer files.
/// A pointer file is a small text file that replaces a large file in a Git repository,
/// containing metadata about the original file and where to find it.
use std::collections::HashMap;
use std::str::FromStr;

use crate::core::{GitError, Result};

/// A Git LFS pointer file
#[derive(Debug, Clone)]
pub struct LfsPointer {
    /// Version string for the LFS spec
    pub version: String,
    
    /// Object ID for the LFS object
    pub oid: String,
    
    /// Size of the file in bytes
    pub size: u64,
    
    /// IPFS CID for the object (if stored in IPFS)
    pub ipfs_cid: Option<String>,
    
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
            attributes: HashMap::new(),
        }
    }
    
    /// Set the IPFS CID for this object
    pub fn set_ipfs_cid(&mut self, cid: &str) {
        self.ipfs_cid = Some(cid.to_string());
        
        // Also store in attributes for compatibility with standard LFS clients
        self.attributes.insert("x-artigit-ipfs-cid".to_string(), cid.to_string());
    }
    
    /// Parse a pointer from a string
    pub fn parse(s: &str) -> Result<Self> {
        let mut version = None;
        let mut oid = None;
        let mut size = None;
        let mut ipfs_cid = None;
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
        
        // Add custom attributes
        for (key, value) in &self.attributes {
            // Skip the IPFS CID attribute as we've already added it
            if key != "x-artigit-ipfs-cid" {
                lines.push(format!("{} {}", key, value));
            }
        }
        
        lines.join("\n")
    }
    
    /// Get the URL for this pointer
    pub fn url(&self, base_url: &str) -> String {
        format!("{}/objects/{}", base_url, self.oid)
    }
    
    /// Check if this pointer can use IPFS for retrieval
    pub fn has_ipfs(&self) -> bool {
        self.ipfs_cid.is_some()
    }
    
    /// Create a pointer from a file
    pub async fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let content = tokio::fs::read_to_string(path).await
            .map_err(|e| GitError::LfsError(format!("Failed to read LFS pointer: {}", e)))?;
            
        Self::parse(&content)
    }
    
    /// Write a pointer to a file
    pub async fn write_to_file(&self, path: impl AsRef<std::path::Path>) -> Result<()> {
        tokio::fs::write(path, self.to_string()).await
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