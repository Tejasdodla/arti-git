use std::fmt;
use std::error::Error;

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
    /// Configuration errors
    Config(String),
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
            GitError::Config(msg) => write!(f, "Configuration error: {}", msg),
        }
    }
}

impl Error for GitError {}

// Convert from other error types
impl From<std::io::Error> for GitError {
    fn from(err: std::io::Error) -> Self {
        GitError::IO(err.to_string())
    }
}

impl From<crate::core::config::ConfigError> for GitError {
    fn from(err: crate::core::config::ConfigError) -> Self {
        GitError::Config(err.to_string())
    }
}