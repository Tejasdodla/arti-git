use std::fmt;
use std::error::Error;

mod object;
mod error;
mod config;
mod client;
mod operations;

pub use object::{ObjectId, ObjectType};
pub use error::{GitError, Result};
pub use config::{ArtiGitConfig, TorConfig, GitConfig, OnionServiceConfig, ConfigError};
pub use client::ArtiGitClient;
pub use operations::{
    FileStatus, FileChange, status, create_branch, list_branches, 
    delete_branch, checkout, log, format_commit
};