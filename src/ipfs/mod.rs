// IPFS integration for ArtiGit
// This module provides basic integration with IPFS for Git object storage and retrieval

mod config;
mod client;
mod storage;

pub use config::IpfsConfig;
pub use client::IpfsClient;
pub use storage::{IpfsObjectStorage, IpfsObjectProvider, IpfsStorageError};

use crate::core::{GitError, Result};

/// Initialize the IPFS module
/// This function should be called before using any IPFS functionality
pub async fn init() -> Result<()> {
    // Ensure IPFS dependencies are available
    // This is just a placeholder for now
    Ok(())
}

/// Check if IPFS is available on the system
pub async fn is_available() -> bool {
    // In a real implementation, this would check for IPFS daemon availability
    // For now, just return true for demonstration purposes
    true
}

/// Convert an IPFS error to a GitError
pub fn convert_error(error: impl std::error::Error) -> GitError {
    GitError::Transport(format!("IPFS error: {}", error))
}