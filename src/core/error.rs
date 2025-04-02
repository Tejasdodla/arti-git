use std::fmt;
use std::error::Error;
use std::path::PathBuf;

/// Result type used throughout the application
pub type Result<T> = std::result::Result<T, GitError>;

/// Git-specific error types
#[derive(Debug)]
pub enum GitError {
    /// IO errors with path context
    IO(String, Option<PathBuf>),
    /// Repository errors with path context
    Repository(String, Option<PathBuf>),
    /// Invalid object ID
    InvalidObjectId(String),
    /// Transport errors with URL context
    Transport(String, Option<String>),
    /// Protocol errors
    Protocol(String),
    /// Cryptography errors
    Crypto(String),
    /// Invalid arguments
    InvalidArgument(String),
    /// Not implemented
    NotImplemented(String),
    /// Configuration errors
    Config(String),
    /// LFS errors
    LfsError(String),
    /// IPFS errors
    IpfsError(String),
    /// Object storage errors
    ObjectStorage(String),
    /// Authentication errors
    Authentication(String),
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitError::IO(msg, Some(path)) => write!(f, "IO error for path '{}': {}", path.display(), msg),
            GitError::IO(msg, None) => write!(f, "IO error: {}", msg),
            GitError::Repository(msg, Some(path)) => write!(f, "Repository error for '{}': {}", path.display(), msg),
            GitError::Repository(msg, None) => write!(f, "Repository error: {}", msg),
            GitError::InvalidObjectId(msg) => write!(f, "Invalid object ID: {}", msg),
            GitError::Transport(msg, Some(url)) => write!(f, "Transport error for URL '{}': {}", url, msg),
            GitError::Transport(msg, None) => write!(f, "Transport error: {}", msg),
            GitError::Protocol(msg) => write!(f, "Protocol error: {}", msg),
            GitError::Crypto(msg) => write!(f, "Crypto error: {}", msg),
            GitError::InvalidArgument(msg) => write!(f, "Invalid argument: {}", msg),
            GitError::NotImplemented(msg) => write!(f, "Not implemented: {}", msg),
            GitError::Config(msg) => write!(f, "Configuration error: {}", msg),
            GitError::LfsError(msg) => write!(f, "LFS error: {}", msg),
            GitError::IpfsError(msg) => write!(f, "IPFS error: {}", msg),
            GitError::ObjectStorage(msg) => write!(f, "Object storage error: {}", msg),
            GitError::Authentication(msg) => write!(f, "Authentication error: {}", msg),
        }
    }
}

impl Error for GitError {}

// Convert from other error types
impl From<std::io::Error> for GitError {
    fn from(err: std::io::Error) -> Self {
        GitError::IO(err.to_string(), None)
    }
}

/// Create an IO error with path context
pub fn io_err(err: impl ToString, path: impl Into<PathBuf>) -> GitError {
    GitError::IO(err.to_string(), Some(path.into()))
}

/// Create a repository error with path context
pub fn repo_err(err: impl ToString, path: impl Into<PathBuf>) -> GitError {
    GitError::Repository(err.to_string(), Some(path.into()))
}

/// Create a transport error with URL context
pub fn transport_err(err: impl ToString, url: impl Into<String>) -> GitError {
    GitError::Transport(err.to_string(), Some(url.into()))
}

impl From<crate::core::config::ConfigError> for GitError {
    fn from(err: crate::core::config::ConfigError) -> Self {
        GitError::Config(err.to_string())
    }
}

#[cfg(feature = "ipfs")]
impl From<ipfs_api_backend_hyper::Error> for GitError {
    fn from(err: ipfs_api_backend_hyper::Error) -> Self {
        GitError::IpfsError(format!("IPFS API error: {}", err))
    }
}

#[cfg(feature = "tor")]
impl From<arti_client::Error> for GitError {
    fn from(err: arti_client::Error) -> Self {
        GitError::Transport(format!("Arti client error: {}", err), None)
    }
}