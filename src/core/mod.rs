use std::fmt;
use std::error::Error;

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

/// Result type used throughout the application
pub type Result<T> = std::result::Result<T, GitError>;

/// Git-specific error types
#[derive(Debug)]
pub enum GitError {
    /// IO errors 
    IO(String),
    /// Repository errors
    Repository(String),
    /// Invalid object ID
    InvalidObjectId(String),
    /// Transport errors
    Transport(String),
    /// Protocol errors
    Protocol(String),
    /// Cryptography errors
    Crypto(String),
    /// Invalid arguments
    InvalidArgument(String),
    /// Not implemented
    NotImplemented(String),
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitError::IO(msg) => write!(f, "IO error: {}", msg),
            GitError::Repository(msg) => write!(f, "Repository error: {}", msg),
            GitError::InvalidObjectId(msg) => write!(f, "Invalid object ID: {}", msg),
            GitError::Transport(msg) => write!(f, "Transport error: {}", msg),
            GitError::Protocol(msg) => write!(f, "Protocol error: {}", msg),
            GitError::Crypto(msg) => write!(f, "Crypto error: {}", msg),
            GitError::InvalidArgument(msg) => write!(f, "Invalid argument: {}", msg),
            GitError::NotImplemented(msg) => write!(f, "Not implemented: {}", msg),
        }
    }
}

impl Error for GitError {}

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