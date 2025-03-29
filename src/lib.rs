// filepath: /workspaces/codespaces-blank/arti-git/src/lib.rs
//! ArtiGit: A Git implementation that integrates with Arti for Tor connectivity
//! and with IPFS for distributed object storage

pub mod core;
pub mod transport;
pub mod repository;
pub mod commands;
pub mod crypto;
pub mod protocol;
pub mod service;
pub mod utils;
pub mod ipfs;

// Re-export main components for easier consumption
pub use core::{
    ArtiGitClient, ArtiGitConfig, GitError, Result, ObjectId, ObjectType,
    TorConfig, GitConfig, OnionServiceConfig, ConfigError,
    FileStatus, FileChange, status, create_branch, list_branches, 
    delete_branch, checkout, log, format_commit
};
pub use service::GitOnionService;
pub use transport::TorTransport;
pub use ipfs::{IpfsClient, IpfsConfig, IpfsObjectStorage, IpfsObjectProvider};

// Version information
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const AUTHORS: &str = env!("CARGO_PKG_AUTHORS");