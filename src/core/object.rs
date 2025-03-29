use std::fmt;
use std::str::FromStr;

use sha1::{Sha1, Digest};
use hex::{FromHex, ToHex};

/// Represents a Git object ID (SHA-1 hash)
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ObjectId([u8; 20]);

impl ObjectId {
    /// Create a new ObjectId from bytes
    pub fn new(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }
    
    /// Compute the object ID for the given content
    pub fn compute(object_type: ObjectType, content: &[u8]) -> Self {
        let mut header = Vec::new();
        header.extend_from_slice(object_type.as_str().as_bytes());
        header.push(b' ');
        header.extend_from_slice(content.len().to_string().as_bytes());
        header.push(0);
        
        let mut hasher = Sha1::new();
        hasher.update(&header);
        hasher.update(content);
        
        let hash = hasher.finalize();
        let mut bytes = [0u8; 20];
        bytes.copy_from_slice(&hash);
        
        Self(bytes)
    }
    
    /// Get the object ID as bytes
    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
    
    /// Get a hex string representation
    pub fn to_hex(&self) -> String {
        self.0.encode_hex::<String>()
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl FromStr for ObjectId {
    type Err = hex::FromHexError;
    
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = <[u8; 20]>::from_hex(s)?;
        Ok(Self(bytes))
    }
}

/// Enumeration of Git object types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Commit,
    Tree,
    Blob,
    Tag,
}

impl ObjectType {
    /// Convert the object type to its string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            ObjectType::Commit => "commit",
            ObjectType::Tree => "tree",
            ObjectType::Blob => "blob",
            ObjectType::Tag => "tag",
        }
    }
    
    /// Try to parse an object type from a string
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "commit" => Some(ObjectType::Commit),
            "tree" => Some(ObjectType::Tree),
            "blob" => Some(ObjectType::Blob),
            "tag" => Some(ObjectType::Tag),
            _ => None,
        }
    }
}