use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// Error types for LFS pointer operations
#[derive(Debug, Error)]
pub enum LfsPointerError {
    #[error("Missing version in LFS pointer")]
    MissingVersion,
    
    #[error("Missing oid in LFS pointer")]
    MissingOid,
    
    #[error("Missing size in LFS pointer")]
    MissingSize,
    
    #[error("Invalid size value: {0}")]
    InvalidSize(String),
    
    #[error("Invalid format in LFS pointer")]
    InvalidFormat,
    
    #[error("Unsupported version: {0}")]
    UnsupportedVersion(String),
}

/// Represents a Git LFS pointer file
#[derive(Debug, Clone)]
pub struct LfsPointer {
    /// LFS spec version
    pub version: String,
    
    /// Object ID in the format "sha256:[hash]"
    pub oid: String,
    
    /// Size of the actual file in bytes
    pub size: u64,
    
    /// Optional IPFS CID for the object
    pub ipfs_cid: Option<String>,
}

impl LfsPointer {
    /// Create a new LFS pointer
    pub fn new(oid: &str, size: u64) -> Self {
        Self {
            version: "https://git-lfs.github.com/spec/v1".to_string(),
            oid: oid.to_string(),
            size,
            ipfs_cid: None,
        }
    }
    
    /// Parse an LFS pointer from a string
    pub fn parse(content: &str) -> Result<Self, LfsPointerError> {
        let mut version = None;
        let mut oid = None;
        let mut size = None;
        let mut ipfs_cid = None;
        
        for line in content.lines() {
            let line = line.trim();
            
            if line.starts_with("version ") {
                version = Some(line[8..].to_string());
            } else if line.starts_with("oid ") {
                oid = Some(line[4..].to_string());
            } else if line.starts_with("size ") {
                size = match line[5..].parse::<u64>() {
                    Ok(s) => Some(s),
                    Err(_) => return Err(LfsPointerError::InvalidSize(line[5..].to_string())),
                };
            } else if line.starts_with("x-ipfs-cid ") {
                ipfs_cid = Some(line[11..].to_string());
            }
        }
        
        let version = version.ok_or(LfsPointerError::MissingVersion)?;
        let oid = oid.ok_or(LfsPointerError::MissingOid)?;
        let size = size.ok_or(LfsPointerError::MissingSize)?;
        
        if !version.starts_with("https://git-lfs.github.com/spec/") {
            return Err(LfsPointerError::UnsupportedVersion(version));
        }
        
        Ok(Self {
            version,
            oid,
            size,
            ipfs_cid,
        })
    }
    
    /// Get the hash part of the oid
    pub fn hash(&self) -> String {
        if let Some(sha_part) = self.oid.strip_prefix("sha256:") {
            sha_part.to_string()
        } else {
            self.oid.clone()
        }
    }
    
    /// Set the IPFS CID for this pointer
    pub fn set_ipfs_cid(&mut self, cid: &str) {
        self.ipfs_cid = Some(cid.to_string());
    }
}

impl fmt::Display for LfsPointer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "version {}", self.version)?;
        writeln!(f, "oid {}", self.oid)?;
        writeln!(f, "size {}", self.size)?;
        
        if let Some(cid) = &self.ipfs_cid {
            writeln!(f, "x-ipfs-cid {}", cid)?;
        }
        
        Ok(())
    }
}

impl FromStr for LfsPointer {
    type Err = LfsPointerError;
    
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}