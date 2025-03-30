/// Git LFS (Large File Storage) implementation with IPFS integration
///
/// This module provides full Git LFS functionality, with added support for
/// using IPFS as a storage backend for large files.

// Internal modules
mod config;
mod client;
mod server;
mod filter;
mod pointer;
mod storage;
mod commands;

// Public exports
pub use config::LfsConfig;
pub use client::LfsClient;
pub use server::LfsServer;
pub use filter::LfsFilter;
pub use pointer::LfsPointer;
pub use storage::{LfsStorage, LfsObjectProvider, LfsObjectId};

use crate::core::{ArtiGitClient, Result};
use std::path::Path;
use std::sync::Arc;

/// Configure Git LFS for a repository
pub async fn configure_lfs(client: &ArtiGitClient, repo_path: impl AsRef<Path>) -> Result<()> {
    // Get the LFS client from ArtiGitClient
    let lfs_client = client.lfs_client()
        .ok_or_else(|| crate::core::GitError::LfsError("LFS is not enabled".to_string()))?;
        
    // Initialize LFS in the repository
    lfs_client.initialize(repo_path).await
}

/// Add a file pattern to be tracked by LFS
pub async fn track(client: &ArtiGitClient, pattern: &str, repo_path: impl AsRef<Path>) -> Result<()> {
    // Get the LFS client from ArtiGitClient
    let lfs_client = client.lfs_client()
        .ok_or_else(|| crate::core::GitError::LfsError("LFS is not enabled".to_string()))?;
        
    // Add the pattern to track
    lfs_client.track(pattern, repo_path).await
}

/// Start an LFS server for handling LFS operations
pub async fn start_server(
    client: &ArtiGitClient, 
    addr: &str, 
    base_url: &str,
    repo_dir: impl AsRef<Path>
) -> Result<()> {
    // Get the LFS client and storage from ArtiGitClient
    let lfs_client = client.lfs_client()
        .ok_or_else(|| crate::core::GitError::LfsError("LFS is not enabled".to_string()))?;
    
    let lfs_storage = client.lfs_storage()
        .ok_or_else(|| crate::core::GitError::LfsError("LFS storage is not available".to_string()))?;
    
    // Create and start the LFS server
    let server = LfsServer::new(lfs_client, lfs_storage, base_url);
    server.start(addr).await
}

/// Install LFS filter into Git config
pub fn install_filter() -> Result<()> {
    filter::install_filter()
}

/// Install LFS filter into a specific repository's Git config
pub fn install_filter_in_repo(repo_path: impl AsRef<Path>) -> Result<()> {
    filter::install_filter_in_repo(repo_path)
}