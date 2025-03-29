// LFS (Large File Storage) module for ArtiGit
// This module provides Git LFS integration with IPFS for efficient handling of large files

mod config;
mod pointer;
mod client;
mod server;
mod storage;
mod filter;

pub use config::LfsConfig;
pub use pointer::{LfsPointer, LfsPointerError};
pub use client::LfsClient;
pub use server::LfsServer;
pub use storage::{LfsStorage, LfsObjectId};
pub use filter::{LfsFilter, install_filter};

use crate::core::{GitError, Result};

/// Initialize the LFS module
pub async fn init() -> Result<()> {
    // Initialize any necessary LFS components
    Ok(())
}

/// Convert an LFS error to a GitError
pub fn convert_error(error: impl std::error::Error) -> GitError {
    GitError::LfsError(format!("LFS error: {}", error))
}