use std::fmt;

use crate::core::{GitError, Result};
use gix::hash::ObjectId as GixObjectId; // Import gitoxide ObjectId

/// Represents a Git object ID (SHA-1 hash)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectId {
    id: [u8; 20],
}

impl ObjectId {
    /// Create a new object ID from a hex string
    pub fn from_hex(hex: &str) -> Result<Self> {
        if hex.len() != 40 {
            return Err(GitError::InvalidObjectId(
                format!("Invalid object ID length: {}", hex.len())
            ));
        }
        
        let mut id = [0u8; 20];
        for i in 0..20 {
            let byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                .map_err(|_| GitError::InvalidObjectId(
                    format!("Invalid hex in object ID: {}", hex)
                ))?;
            id[i] = byte;
        }
        
        Ok(Self { id })
    }
    
    /// Get the hex string representation of this object ID
    pub fn to_hex(&self) -> String {
        self.id.iter().fold(String::new(), |mut s, b| {
            use std::fmt::Write;
            write!(s, "{:02x}", b).unwrap();
            s
        })
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

// Implement conversion from gitoxide's ObjectId
impl From<GixObjectId> for ObjectId {
    fn from(gix_oid: GixObjectId) -> Self {
        ObjectId {
            id: *gix_oid.as_bytes(), // gix_oid.as_bytes() returns &[u8; 20]
        }
    }
}

/// Git object types
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectType {
    /// Blob object (file content)
    Blob,
    /// Tree object (directory)
    Tree,
    /// Commit object
    Commit,
    /// Tag object
    Tag,
}

impl ObjectType {
    /// Convert to a string representation
    pub fn to_str(&self) -> &'static str {
        match self {
            ObjectType::Blob => "blob",
            ObjectType::Tree => "tree",
            ObjectType::Commit => "commit",
            ObjectType::Tag => "tag",
        }
    }
    
    /// Convert from a string representation
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "blob" => Ok(ObjectType::Blob),
            "tree" => Ok(ObjectType::Tree),
            "commit" => Ok(ObjectType::Commit),
            "tag" => Ok(ObjectType::Tag),
            _ => Err(GitError::InvalidArgument(
                format!("Invalid object type: {}", s)
            )),
        }
    }
}