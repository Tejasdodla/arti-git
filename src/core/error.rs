use std::path::PathBuf;
use thiserror::Error;

use super::object::ObjectId;

/// Common error types for Git operations
#[derive(Debug, Error)]
pub enum GitError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Object not found: {0}")]
    NotFound(ObjectId),
    
    #[error("Invalid Git object: {0}")]
    InvalidObject(String),
    
    #[error("Transport error: {0}")]
    Transport(String),
    
    #[error("Reference error: {0}")]
    Reference(String),
    
    #[error("Path error: {0}")]
    Path(PathBuf),
    
    #[error("Index error: {0}")]
    Index(String),
    
    #[error("Config error: {0}")]
    Config(String),
}

/// Result type for Git operations
pub type Result<T> = std::result::Result<T, GitError>;